@echo off
rem 编译脚本：Rust 的 windows-gnu 目标需要 binutils（dlltool / as），
rem 这里把本地 mingw64\bin 与 cargo 一起补进 PATH 再调用 cargo。
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%USERPROFILE%\mingw64\bin;%PATH%"
cd /d "%~dp0"
cargo build --release %*
if errorlevel 1 (
    echo.
    echo 编译失败。
    exit /b 1
)
echo.
echo 编译完成：%~dp0target\release\procman.exe
endlocal
