@echo off
rem Usage: docker\publish.cmd [--restart]
rem
rem Run from cmd or double-click. Finds Git Bash and runs docker/publish.sh
rem (which checks Docker Desktop, builds the agt-dev image if missing, then
rem compiles and installs). See docker/publish.sh for the options.
rem
rem Keep this file ASCII-only: cmd misreads multi-byte (e.g. Chinese) lines in
rem batch files, even after "chcp 65001", and runs fragments of them as commands.
rem Messages for the user are printed by publish.sh instead.
setlocal
for /f "tokens=2 delims=:" %%a in ('chcp') do set "OLDCP=%%a"
set "OLDCP=%OLDCP: =%"
set "OLDCP=%OLDCP:.=%"
rem UTF-8 console so the Chinese output of publish.sh displays correctly
chcp 65001 >nul

rem Double-clicked (cmd /c this file): pause at the end so the window stays open
set "PAUSE_AT_END="
echo %CMDCMDLINE% | find /i "%~nx0" >nul && set "PAUSE_AT_END=1"

cd /d "%~dp0.." || goto :fail

rem --- Find Git Bash (not the bash on PATH, which may be WSL's System32\bash.exe)
set "BASH="
for %%p in ("%ProgramFiles%\Git\bin\bash.exe" "%ProgramW6432%\Git\bin\bash.exe" "%LocalAppData%\Programs\Git\bin\bash.exe") do (
  if not defined BASH if exist "%%~p" set "BASH=%%~p"
)
if not defined BASH (
  for /f "delims=" %%g in ('where git 2^>nul') do (
    if not defined BASH if exist "%%~dpg..\bin\bash.exe" set "BASH=%%~dpg..\bin\bash.exe"
  )
)
if not defined BASH (
  echo Git Bash ^(bash.exe^) not found. Install Git for Windows: https://git-scm.com/download/win
  goto :fail
)

"%BASH%" docker/publish.sh %*
set "RC=%ERRORLEVEL%"
goto :end

:fail
set "RC=1"

:end
if defined PAUSE_AT_END pause
if defined OLDCP chcp %OLDCP% >nul
endlocal & exit /b %RC%
