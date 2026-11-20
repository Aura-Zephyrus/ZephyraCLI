@echo off
rem Windows launcher: bypasses PowerShell execution policy, downloads and runs install.ps1.
rem Usage:  install.cmd                   install latest
rem         install.cmd -Version v0.1.0   install a specific version
rem         install.cmd -Uninstall        uninstall
rem         install.cmd -NoPath           do not modify PATH
setlocal EnableExtensions

set "URL=https://raw.githubusercontent.com/Aura-Zephyrus/ZephyraCLI/main/install.ps1"
set "PS=%TEMP%\zephyra-install-%RANDOM%.ps1"

powershell -NoProfile -ExecutionPolicy Bypass -Command "Invoke-WebRequest -Uri '%URL%' -OutFile '%PS%'"
if errorlevel 1 (
    echo [zephyra] failed to download the installer
    exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%PS%" %*
set "RC=%ERRORLEVEL%"
del "%PS%" >nul 2>&1
exit /b %RC%
