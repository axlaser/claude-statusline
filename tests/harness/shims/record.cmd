@echo off
rem Windows PATH shim, installed as powershell.cmd. The status line
rem spawns its detached notification with `Start-Process -FilePath 'powershell'`,
rem which resolves through PATH, so a shim directory prepended to PATH sees the
rem spawn before System32 does.
rem
rem The whole command line is recorded as one line rather than one field per
rem argument: cmd.exe cannot split %* without re-parsing quotes, and re-parsing
rem would record what cmd thinks was passed instead of what the status line
rem actually passed. The isolated HOME's notify.ps1 recorder captures the same
rem spawn field-by-field, so nothing depends on this file's fidelity alone.
if not defined STATUSLINE_CAPTURE_FILE goto :done
>>"%STATUSLINE_CAPTURE_FILE%" echo powershell	%*
:done
exit /b 0
