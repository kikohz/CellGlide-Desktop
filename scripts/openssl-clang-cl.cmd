@echo off
rem The C compiler for the vendored OpenSSL on aarch64-pc-windows-msvc (feature `wireless`), used
rem instead of cl: MSVC's ARM64 compiler at /O2 builds an OpenSSL whose TLS 1.2 handshake crashes
rem (see crates\cg-platform\build.rs). openssl-src runs OpenSSL's Configure, which takes its compiler
rem from CC, so: set CC to the full path of this file, and CC_aarch64_pc_windows_msvc=clang-cl so that
rem the other C builds (cc-rs) call clang-cl directly.
rem clang-cl keeps the debug info in each object and writes no ossl_static.pdb, which OpenSSL's
rem install step copies; inside the OpenSSL build tree an empty one is left for it.
clang-cl --target=arm64-pc-windows-msvc %*
if errorlevel 1 exit /b 1
if exist util\copy.pl if not exist ossl_static.pdb type nul > ossl_static.pdb
exit /b 0
