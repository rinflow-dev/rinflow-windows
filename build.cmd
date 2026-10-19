@echo off
rem Runs build.ps1 without changing the machine's PowerShell execution policy.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
exit /b %ERRORLEVEL%
