# lprofile-rs
A simple Lua 5.3 profiler module, written in Rust.

Supports recursive (and tail-recursive) functions. Should work fine with
`errors` getting thrown around. Does not support coroutines, however.

## Building
Install `cargo` and run:

```
$ cargo build --release
```

Look for the shared library in the `target/release/` directory.

## Usage
`require("liblprofile")` returns a table with 1 field: `Profiler`, which is a
function that creates a profiler instance when called.

To start a profiling session, call that instance again, passing the function to
profile. After the session finishes, you'll get a table with the profiling data.

Please note the table is not sorted.

### Non-integer keys
- `totalTime`: the total time elapsed, in seconds.

### Integer keys
Under integer keys, the table stores profiling data for each tracked function as
another table. It has the following fields:

- `name`: the name of the function, if available.
- `totalTime`: the time spent running the function.
- `totalSelfTime`: the time spent running the function's body, excluding calls
  to other functions.
- `calls`: the number of times the function was called.

### Table example
```lua
{
  totalTime = 1.212853758,
  {
    name = "anonymous C function ([C])",
    calls = 1,
    totalTime = 0.000012,
    totalSelfTime = 0.000012,
  },
  {
    name = "global C function print ([C])",
    calls = 1,
    totalTime = 0.000037,
    totalSelfTime = 0.000024,
  },
  {
    name = "upvalue Lua function g (examples/hello-world.lua:13)",
    calls = 262143,
    totalTime = 1.105267,
    totalSelfTime = 0.483915,
  },
  {
    name = "upvalue Lua function f (examples/hello-world.lua:5)",
    calls = 262144,
    totalTime = 1.105345,
    totalSelfTime = 0.496909,
  },
  {
    name = "anonymous Lua function (examples/hello-world.lua:21)",
    calls = 1,
    totalTime = 1.212836,
    totalSelfTime = 0.107430,
  },
}
```

## Examples
See `examples/`.
