@echo off
rem Windows launcher: bypasses PowerShell execution policy, downloads and runs update.ps1.
rem   update.cmd                       update to latest
rem   update.cmd -Version v0.1.0       update to a specific version
rem   update.cmd -BaseUrl file:///C:/pkg   update from a local package (dev test)
rem   update.cmd -NoPath -NoMcp        binaries only
setlocal EnableExtensions

set "URL=https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/update.ps1"
set "PS=%TEMP%\zephyra-update-%RANDOM%.ps1"

powershell -NoProfile -ExecutionPolicy Bypass -Command "Invoke-WebRequest -Uri '%URL%' -OutFile '%PS%'"
if errorlevel 1 (
    echo [zephyra] failed to download the updater
    exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%PS%" %*
set "RC=%ERRORLEVEL%"
del "%PS%" >nul 2>&1
exit /b %RC%
