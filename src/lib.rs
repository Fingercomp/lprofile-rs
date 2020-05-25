use std::collections::HashMap;
use std::ffi::CString;
use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::Instant;

use lua::{lua_func, State, Function};
use lua::ffi::{lua_Debug, self};
use lua::libc::c_int;
use once_cell::sync::Lazy;

macro_rules! __fill_table {
    ($state:expr, $key:expr => |$s:ident| $value:expr, $( $tail:tt )*) => {
        let $s = &mut *$state;
        $s.push($key);
        $value;
        $s.set_table(-3);

        __fill_table!($s, $( $tail )*);
    };

    ($state:expr, $key:expr => |$s:ident| $value:expr) => {
        __fill_table!($state, $key => |$s| $value,);
    };

    ($state:expr,) => ();
}

macro_rules! __count_fields {
    ($key:expr => |$state:ident| $value:expr, $( $tail:tt )*) => (1 + __count_fields!($( $tail )*));
    ($key:expr => |$state:ident| $value:expr) => (__count_fields!($key => $value));
    () => (0);
}

macro_rules! table {
    ($state:expr, $( $fields:tt )*) => {{
        let count = __count_fields!($( $fields )*);
        $state.create_table(0, count);
        __fill_table!($state, $( $fields )*);
    }};

    ($state:expr) => (table!($state,));
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FunctionKey(usize);

impl FunctionKey {
    fn from_ar(state: &mut State, ar: *mut lua_Debug) -> Option<Self> {
        let what = CString::new("f").unwrap();

        match unsafe { ffi::lua_getinfo(state.as_ptr(), what.as_ptr(), ar) } {
            0 => None,
            _ => {
                let addr = state.to_pointer(1) as usize;
                state.pop(1);

                Some(Self(addr))
            }
        }
    }
}

struct ProfileData {
    calls: usize,
    total_time: usize,
    total_self_time: usize,
}

struct CallFrame {
    entry: Instant,
    level: usize,
}

struct Profiler {
    data: HashMap<FunctionKey, ProfileData>,
    stack: Vec<CallFrame>,
}

impl Profiler {
    const TYPE_NAME: &'static str = "Profiler";

    fn new(state: &mut State) -> i32 {
        static METATABLE_INIT: Once = Once::new();

        METATABLE_INIT.call_once(|| {
            table!(state,
                "__index" => |s| s.push(lua_func!(Profiler::index)),
                "__gc" => |s| s.push(lua_func!(Profiler::gc)),
            );

            state.new_metatable(Self::TYPE_NAME);
        });

        unsafe {
            *state.new_userdata_typed() = ManuallyDrop::new(Profiler {
                data: HashMap::new(),
                stack: Vec::new(),
            });
        }

        state.set_metatable_from_registry(Self::TYPE_NAME);

        1
    }

    fn index(state: &mut State) -> i32 {
        0
    }

    fn gc(state: &mut State) -> i32 {
        0
    }
}

static LIBRARY: Lazy<Box<[(&str, Function)]>> = Lazy::new(|| {
    Box::new([
        ("Profiler", lua_func!(Profiler::new)),
    ])
});

#[no_mangle]
pub extern "C" fn luaopen_liblprofile_hook(state: *mut ffi::lua_State) -> c_int {
    let mut state = unsafe { lua::State::from_ptr(state) };
    state.new_lib(&LIBRARY);

    1
}
