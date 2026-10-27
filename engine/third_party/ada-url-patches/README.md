# Ada URL build patch

Published ada-url 4.0.0, exact crate SHA in UPSTREAM. Only the MSVC build recipe
is changed: use Rust target crt-static instead of unconditionally compiling /MT,
and remove the global /NODEFAULTLIB:libcmt directive. This parser owns and frees
its own C++ allocations, but still must share the embedding product CRT policy.
URLPattern remains disabled by the upstream build (ADA_INCLUDE_URL_PATTERN=0).
The iOS native closure does not depend on runtime-v8 or this parser; WebKit uses
its native URL classes. Non-Linux builds still require their native CI evidence.
