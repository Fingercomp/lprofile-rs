use std::collections::HashMap;
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::Instant;

use lua::{lua_func, State, Function, Hook, HookMask};
use lua::ffi::{lua_Debug, self};
use lua::libc::c_int;
use once_cell::sync::Lazy;

macro_rules! __fill_table {
    ($state:expr; $key:expr => |$s:ident| $value:expr, $( $tail:tt )*) => {
        let $s = &mut *$state;
        $s.push($key);
        $value;
        $s.set_table(-3);

        __fill_table!($s, $( $tail )*);
    };

    ($state:expr; $key:expr => |$s:ident| $value:expr) => {
        __fill_table!($state, $key => |$s| $value,);
    };

    ($state:expr;) => ();
}

macro_rules! __count_fields {
    ($key:expr => |$state:ident| $value:expr, $( $tail:tt )*) => (1 + __count_fields!($( $tail )*));
    ($key:expr => |$state:ident| $value:expr) => (__count_fields!($key => $value));
    () => (0);
}

macro_rules! table {
    ($state:expr; $( $fields:tt )*) => {{
        let count = __count_fields!($( $fields )*);
        $state.create_table(0, count);
        __fill_table!($state, $( $fields )*);
    }};

    ($state:expr;) => (table!($state,));
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FunctionKey(usize);

impl FunctionKey {
    // Safety: ar must be a valid pointer to an activation record received by a hook
    unsafe fn from_ar(state: &mut State, ar: *mut lua_Debug) -> Option<Self> {
        let what = CString::new("f").unwrap();

        match ffi::lua_getinfo(state.as_ptr(), what.as_ptr(), ar) {
            0 => None,
            _ => {
                let addr = state.to_pointer(1) as usize;
                state.pop(1);

                Some(Self(addr))
            }
        }
    }
}

struct ProfileEntry {
    calls: usize,
    total_time: usize,
    total_self_time: usize,
}

struct CallFrame {
    entry: Instant,
    level: usize,
    key: FunctionKey,
}

struct ProfilingResult {
    data: HashMap<FunctionKey, ProfileEntry>,
}

impl ProfilingResult {
    const TYPE_NAME: &'static str = "ProfilingResult";

    fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    fn move_to_lua(self, state: &mut State) -> i32 {
        static METATABLE: Once = Once::new();

        METATABLE.call_once(|| {
            state.new_metatable(Self::TYPE_NAME);
            state.set_fns(&[
                ("__index", lua_func!(Self::index)),
                ("__gc", lua_func!(Self::gc)),
            ], 0);
        });

        // Safety: guaranteed by Lua
        unsafe {
            *state.new_userdata_typed() = ManuallyDrop::new(self);
        }

        state.set_metatable_from_registry(Self::TYPE_NAME);

        1
    }

    fn index(state: &mut State) -> i32 {
        0
    }

    fn gc(state: &mut State) -> i32 {
        unsafe {
            // Safety: guaranteed by Lua (technically there's debug.getmetatable, but I'm not
            // concerned with that).
            let this: &mut ManuallyDrop<Self> = state.check_userdata_typed(1, Self::TYPE_NAME);

            // Safety: gc is guaranteed to be called only once; also see above.
            ManuallyDrop::drop(this);
        }

        state.pop(1);

        0
    }
}

struct Profiler {
    result: Option<ProfilingResult>,
    stack: Vec<CallFrame>,
}

impl Profiler {
    const TYPE_NAME: &'static str = "Profiler";

    fn new(state: &mut State) -> i32 {
        static METATABLE: Once = Once::new();

        METATABLE.call_once(|| {
            state.new_metatable(Self::TYPE_NAME);
            state.set_fns(&[
                ("__call", lua_func!(Self::call)),
                ("__gc", lua_func!(Self::gc)),
            ], 0);
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
        state.check_userdata(1, Self::TYPE_NAME);
        state.check_type(2, lua::Type::Function);

        let prev_hook = Self::set_hook(state);

        // Safety: checked above; set_hook does not modify the stack.
        let this: &mut Self = unsafe { state.to_userdata_typed(1).unwrap() };

        this.result.replace(ProfilingResult {
            data: HashMap::new(),
        });

        let status = state.pcall(0, 0, 0);

        Self::unset_hook(state, prev_hook);

        if status.is_err() {
            // propagate the error
            state.error();
        }

        // Safety: checked above; unset_hook or pcall did not modify the stack if we're here.
        let this: &mut Self = unsafe { state.to_userdata_typed(1).unwrap() };
        let result = this.result.take().unwrap();
        state.pop(1);
        result.move_to_lua(state);

        1
    }

    fn gc(state: &mut State) -> i32 {
        0
    }

    fn set_hook(state: &mut State) -> (Hook, HookMask, c_int) {
        let prev = (state.get_hook(), state.get_hook_mask(), state.get_hook_count());

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
        todo!()
    }
}

static LIBRARY: Lazy<Box<[(&str, Function)]>> = Lazy::new(|| {
    Box::new([
        ("Profiler", lua_func!(Profiler::new)),
    ])
});

// Safety: must only be called using Lua's require.
#[no_mangle]
pub unsafe extern "C" fn luaopen_liblprofile_hook(state: *mut ffi::lua_State) -> c_int {
    let mut state = lua::State::from_ptr(state);
    state.new_lib(&LIBRARY);

    1
}
