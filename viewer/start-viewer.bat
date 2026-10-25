@echo off
setlocal
cd /d "%~dp0.."
if not exist "runtime\bin\antmemory.exe" (
  echo Extract the ANTmemory release ZIP here first.
  pause
  exit /b 1
)
"runtime\bin\antmemory.exe" viewer
if errorlevel 1 pause
