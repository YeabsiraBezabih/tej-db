# tej-db-ffi

C ABI and Foreign Function Interface (FFI) bindings for `tej-db`.

Provides zero-copy, panic-safe C functions suitable for:
- Flutter (`dart:ffi`) on iOS, Android, macOS, Linux, and Windows.
- C/C++ native applications.
- Any language supporting C FFI (Python, Zig, Node.js, etc.).

All exported functions catch panics internally and return integer status codes, preventing process abortion on runtime exceptions.
