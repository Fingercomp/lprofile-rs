use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fmt::{self, Display};
use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::{Duration, Instant};

use lua::ffi::{self, lua_Debug};
use lua::libc::c_int;
use lua::{lua_func, Function, Hook, HookMask, State};
use once_cell::sync::Lazy;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FunctionKey(usize);

impl FunctionKey {
    // Safety: ar must be a valid pointer to an activation record received by a hook
    unsafe fn from_ar(state: &mut State, ar: &mut lua_Debug) -> Option<Self> {
        let what = CString::new("f").unwrap();

        match ffi::lua_getinfo(state.as_ptr(), what.as_ptr(), ar) {
            0 => None,
            _ => {
                let addr = state.to_pointer(-1) as usize;
                state.pop(1);

                Some(Self(addr))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FunctionName {
    name: Option<String>,
    function_type: Option<String>,
    source: String,
    line: Option<usize>,
    // Lua function / C function / main chunk
    domain: String,
}

impl FunctionName {
    // Safety: assumes lua_getinfo(L, "nS", ar) has been called.
    unsafe fn fill_from(ar: &lua_Debug) -> Self {
        let name = if ar.name.is_null() {
            None
        } else {
            Some(CStr::from_ptr(ar.name).to_string_lossy().into_owned())
        };

        let function_type = CStr::from_ptr(ar.namewhat).to_string_lossy().into_owned();
        let function_type = if function_type.is_empty() {
            None
        } else {
            Some(function_type)
        };

        let source = CStr::from_ptr(&ar.short_src as *const lua::libc::c_char)
            .to_string_lossy()
            .into_owned();

        let line = ar.linedefined;
        let line = if line == -1 {
            None
        } else {
            Some(line as usize)
        };

        let domain = CStr::from_ptr(ar.what).to_str().unwrap().to_owned();

        Self {
            name,
            function_type,
            source,
            line,
            domain,
        }
    }
}

impl Display for FunctionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.domain == "main" {
            write!(f, "main chunk of {} ({}", self.source, self.source)?;

            if let Some(line) = self.line {
                write!(f, ":{}", line)?;
            }

            write!(f, ")")
        } else {
            if self.name.is_none() {
                write!(f, "anonymous ")?;
            }

            if let Some(ref t) = self.function_type {
                write!(f, "{} ", t)?;
            }

            write!(f, "{} ", self.domain)?;

            if let Some(ref name) = self.name {
                write!(f, "function {} ", name)?;
            } else {
                write!(f, "function ")?;
            }

            write!(f, "({}", self.source)?;

            if let Some(line) = self.line {
                write!(f, ":{}", line)?;
            }

            write!(f, ")")
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProfileEntry {
    calls: usize,
    total_time: Duration,
    total_self_time: Duration,
    name: Option<FunctionName>,
    recursion_depth: usize,
}

impl ProfileEntry {
    fn new(name: Option<FunctionName>) -> Self {
        Self {
            calls: 1,
            total_time: Duration::new(0, 0),
            total_self_time: Duration::new(0, 0),
            name,
            recursion_depth: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CallFrame {
    entry: Instant,
    inner_start: Instant,
    level: usize,
    key: FunctionKey,
    suspended: bool,
}

impl CallFrame {
    fn new(level: usize, key: FunctionKey) -> Self {
        Self {
            entry: Instant::now(),
            inner_start: Instant::now(),
            level,
            key,
            suspended: false,
        }
    }

    fn close(&self, result: &mut ProfilingResult) {
        assert!(!self.suspended, "attempted to close a suspended call frame");

        let entry = result.data.get_mut(&self.key).unwrap();
        entry.total_self_time += self.inner_start.elapsed();

        entry.recursion_depth -= 1;

        if entry.recursion_depth == 0 {
            entry.total_time += self.entry.elapsed();
        }
    }

    fn suspend(&mut self, result: &mut ProfilingResult) {
        assert!(!self.suspended, "the call frame is already suspended");

        let entry = result.data.get_mut(&self.key).unwrap();
        entry.total_self_time += self.inner_start.elapsed();
        self.suspended = true;
    }

    fn resume(&mut self) {
        if !self.suspended {
            return;
        }

        self.inner_start = Instant::now();
        self.suspended = false;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProfilingResult {
    data: HashMap<FunctionKey, ProfileEntry>,
    total_time: Option<Duration>,
}

impl ProfilingResult {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            total_time: None,
        }
    }

    fn move_to_lua(self, state: &mut State) -> i32 {
        let len = self.data.len() as i32;
        state.create_table(len, 1);

        for (i, v) in self.data.values().enumerate() {
            state.create_table(0, 4);

            state.push("name");
            state.push(v.name.as_ref().map_or_else(String::new, |v| v.to_string()));
            state.set_table(-3);

            state.push("calls");
            state.push(v.calls as i64);
            state.set_table(-3);

            state.push("totalTime");
            state.push(v.total_time.as_secs_f64());
            state.set_table(-3);

            state.push("totalSelfTime");
            state.push(v.total_self_time.as_secs_f64());
            state.set_table(-3);

            state.seti(-2, (i + 1) as i64);
        }

        state.push("totalTime");
        state.push(self.total_time.map(|v| v.as_secs_f64()));
        state.set_table(-3);

        1
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Profiler {
    result: Option<ProfilingResult>,
    stack: Vec<CallFrame>,
}

impl Profiler {
    const TYPE_NAME: &'static str = "Profiler";
    const OPAQUE_REGISTRY_KEY: *const i32 = &0 as *const i32;

    fn new(state: &mut State) -> i32 {
        static METATABLE: Once = Once::new();

        METATABLE.call_once(|| {
            state.new_metatable(Self::TYPE_NAME);
            state.set_fns(
                &[
                    ("__call", lua_func!(Self::call)),
                    ("__gc", lua_func!(Self::gc)),
                ],
                0,
            );
        });

        // Safety: guaranteed by Lua.
        unsafe {
            *state.new_userdata_typed() = ManuallyDrop::new(Profiler {
                result: None,
                stack: Vec::new(),
            });
        }

        state.set_metatable_from_registry(Self::TYPE_NAME);

        1
    }

    fn call(state: &mut State) -> i32 {
        // check but don't use, since we need state later
        state.set_top(2);
        state.check_userdata(1, Self::TYPE_NAME);
        state.check_type(2, lua::Type::Function);

        if Self::get_from_registry(state) {
            state.push("attempt to run multiple profiling sessions simulatenously");
            state.error();
        }

        // Safety: checked above; set_hook does not modify the stack.
        let this: &mut ManuallyDrop<Self> = unsafe { state.to_userdata_typed(1).unwrap() };
        this.result.replace(ProfilingResult::new());

        // Stack:
        // BEFORE      AFTER
        // 1    2      1 2
        // Self f      f Self
        state.rotate(1, 1);
        state.raw_setp(lua::REGISTRYINDEX, Self::OPAQUE_REGISTRY_KEY);

        let prev_hook = Self::set_hook(state);

        let start = Instant::now();
        let status = state.pcall(0, 0, 0);
        let total_time = start.elapsed();

        Self::unset_hook(state, prev_hook);

        if status.is_err() {
            // propagate the error
            state.error();
        }

        Self::get_from_registry(state);

        // Safety: the registry is not modified during profiling
        let this: &mut ManuallyDrop<Self> = unsafe { state.to_userdata_typed(-1).unwrap() };
        let mut result = this.result.take().unwrap();
        result.total_time = Some(total_time);
        result.move_to_lua(state)
    }

    fn get_from_registry(state: &mut State) -> bool {
        let result = match state.raw_getp(lua::REGISTRYINDEX, Self::OPAQUE_REGISTRY_KEY) {
            lua::Type::Userdata => !state.test_userdata(-1, Self::TYPE_NAME).is_null(),
            _ => false,
        };

        if !result {
            state.pop(1);
        }

        result
    }

    fn gc(state: &mut State) -> i32 {
        // Safety: guaranteed by Lua unless violated with debug.getmetatable, which is irrelevant.
        unsafe {
            let this: &mut ManuallyDrop<Self> = state.check_userdata_typed(1, Self::TYPE_NAME);
            ManuallyDrop::drop(this);
        }

        state.pop(1);

        0
    }

    fn set_hook(state: &mut State) -> (Hook, HookMask, c_int) {
        let prev = (
            state.get_hook(),
            state.get_hook_mask(),
            state.get_hook_count(),
        );

        let mut mask = HookMask::empty();
        mask.insert(lua::MASKRET);
        mask.insert(lua::MASKCALL);

        state.set_hook(Some(Self::hook), mask, 0);

        prev
    }

    fn unset_hook(state: &mut State, prev: (Hook, HookMask, c_int)) {
        state.set_hook(prev.0, prev.1, prev.2);
    }

    extern "C" fn hook(state: *mut ffi::lua_State, ar: *mut ffi::lua_Debug) {
        // Safety: guaranteed by Lua
        let ar = unsafe { ar.as_mut().unwrap() };
        let state = unsafe { &mut State::from_ptr(state) };

        match ar.event {
            ffi::LUA_HOOKCALL | ffi::LUA_HOOKTAILCALL => Self::call_event(state, ar),
            ffi::LUA_HOOKRET => Self::return_event(state),
            _ => unreachable!(),
        }
    }

    fn get_stack_level(state: &mut State) -> usize {
        let mut level = 2;

        loop {
            if state.get_stack(level).is_none() {
                return (level - 1) as usize;
            }

            level += 1;
        }
    }

    // This function makes sure the call levels are non-descreasing in the stack. `error` may break
    // the profiler otherwise.
    fn set_stack_to(&mut self, level: usize) {
        while let Some(v) = self.stack.last() {
            if v.level <= level {
                // the new frame is not below this entry in the stack
                return;
            }

            // this frame was closed, but the hook was not notified (the stack was unwound)
            let mut v = self.stack.pop().unwrap();
            v.resume();
            v.close(self.result.as_mut().unwrap());
        }
    }

    fn determine_name_for(state: &mut State, ar: &mut lua_Debug) -> Option<FunctionName> {
        let what = CString::new("nS").unwrap();

        // Safety: `what` is valid; `state` and `ar` are valid due to &mut's guarantees
        match unsafe { ffi::lua_getinfo(state.as_ptr(), what.as_ptr(), ar) } {
            0 => None,
            _ => {
                // Safety: the prescribed requirement is fulfilled.
                Some(unsafe { FunctionName::fill_from(ar) })
            }
        }
    }

    fn call_event(state: &mut State, ar: &mut ffi::lua_Debug) {
        // Safety: the activation record is passed to the hook
        let key = unsafe { FunctionKey::from_ar(state, ar).unwrap() };
        let level = Self::get_stack_level(state);

        assert!(Self::get_from_registry(state));
        // Safety: the check above
        let this: &mut ManuallyDrop<Self> = unsafe { state.to_userdata_typed(-1).unwrap() };
        let this: &mut Self = &mut **this;

        if let Some(last) = this.stack.last_mut() {
            last.suspend(this.result.as_mut().unwrap());
        }

        let entry = this
            .result
            .as_mut()
            .unwrap()
            .data
            .entry(key)
            .and_modify(|entry| {
                entry.calls += 1;

                entry.recursion_depth += 1;
            })
            .or_insert_with(|| ProfileEntry::new(None));

        let name = if entry.name.is_none() {
            Self::determine_name_for(state, ar)
        } else {
            None
        };

        let this: &mut ManuallyDrop<Self> = unsafe { state.to_userdata_typed(-1).unwrap() };
        let entry = this.result.as_mut().unwrap().data.get_mut(&key).unwrap();

        if name.is_some() {
            entry.name = name;
        }

        let frame = CallFrame::new(level, key);
        this.stack.push(frame);
    }

    fn return_event(state: &mut State) {
        // Safety: the activation record is passed to the hook
        let level = Self::get_stack_level(state);

        assert!(Self::get_from_registry(state));
        // Safety: the check above
        let this: &mut ManuallyDrop<Self> = unsafe { state.to_userdata_typed(-1).unwrap() };
        this.set_stack_to(level);

        while let Some(frame) = this.stack.last() {
            if frame.level != level {
                break;
            }

            let mut frame = this.stack.pop().unwrap();
            frame.resume();
            frame.close(this.result.as_mut().unwrap());
        }

        if let Some(last) = this.stack.last_mut() {
            last.resume();
        }
    }
}

static LIBRARY: Lazy<Box<[(&str, Function)]>> =
    Lazy::new(|| Box::new([("Profiler", lua_func!(Profiler::new))]));

// Safety: must only be called using Lua's require.
#[no_mangle]
pub unsafe extern "C" fn luaopen_liblprofile_hook(state: *mut ffi::lua_State) -> c_int {
    let mut state = lua::State::from_ptr(state);
    state.new_lib(&LIBRARY);

    1
}
