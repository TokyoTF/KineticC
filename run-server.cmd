@echo off
REM Builds and runs the server, keeping its console output in this window.
REM
REM The MSVC environment has to be loaded first: the Rust toolchain on Windows links with the
REM link.exe that ships with the Visual Studio C++ build tools.
REM
REM Extra arguments are forwarded to the server, so `run-server.cmd --log=trace` works.

setlocal

set "VCVARS="

if exist "%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat"
if not defined VCVARS if exist "%ProgramFiles%\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%ProgramFiles%\Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat"
if not defined VCVARS if exist "%ProgramFiles%\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%ProgramFiles%\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
if not defined VCVARS if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if not defined VCVARS if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat" set "VCVARS=%ProgramFiles(x86)%\Microsoft Visual Studio\2019\BuildTools\VC\Auxiliary\Build\vcvars64.bat"

if not defined VCVARS (
    echo Could not find vcvars64.bat.
    echo Install the Visual Studio C++ build tools, or edit this script to point at them.
    pause
    exit /b 1
)

call "%VCVARS%" >nul
if errorlevel 1 (
    echo Failed to load the MSVC environment from "%VCVARS%".
    pause
    exit /b 1
)

cargo run -- %*
set "EXIT_CODE=%ERRORLEVEL%"

echo.
echo Server exited with code %EXIT_CODE%.
pause
exit /b %EXIT_CODE%
