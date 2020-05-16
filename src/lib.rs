use lua::ffi;

#[no_mangle]
pub extern "C" fn luaopen_liblprofile_hook(state: *mut ffi::lua_State) -> i32 {
    let mut state = unsafe { lua::State::from_ptr(state) };
    state.push_string("hello from Rust!");
    return 1;
}
