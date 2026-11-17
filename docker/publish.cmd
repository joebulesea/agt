@echo off
rem Lines before "chcp 65001" must stay ASCII-only (cmd parses them with the old code page).
setlocal
for /f "tokens=2 delims=:" %%a in ('chcp') do set "OLDCP=%%a"
set "OLDCP=%OLDCP: =%"
set "OLDCP=%OLDCP:.=%"
chcp 65001 >nul

rem 用法：docker\publish.cmd [--restart]
rem
rem 在 cmd（或雙擊）執行 docker/publish.sh：找 Git Bash 的 bash.exe、確認 Docker Desktop 在執行、
rem 沒有 agt-dev 映像就先建置，再把參數原樣交給 publish.sh。參數說明見 publish.sh。
rem 本檔為 UTF-8 + CRLF；結束時還原原本的字碼頁。

rem 雙擊執行時（cmd /c 本檔）結束前暫停，讓視窗不會一閃而過
set "PAUSE_AT_END="
echo %CMDCMDLINE% | find /i "%~nx0" >nul && set "PAUSE_AT_END=1"

cd /d "%~dp0.." || goto :fail

rem --- 找 Git Bash（不用 PATH 上的 bash，避免抓到 WSL 的 C:\Windows\System32\bash.exe）
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
  echo 找不到 Git Bash（bash.exe）。請安裝 Git for Windows：https://git-scm.com/download/win
  goto :fail
)

rem --- Docker Desktop
docker info >nul 2>&1
if errorlevel 1 (
  echo Docker Desktop 沒有在執行，請先開啟後再試一次。
  goto :fail
)

rem --- agt-dev 映像（第一次才需要建置）
docker image inspect agt-dev >nul 2>&1
if errorlevel 1 (
  echo ==^> 建置 agt-dev 映像（第一次執行，需要幾分鐘）
  docker build -t agt-dev docker || goto :fail
)

"%BASH%" docker/publish.sh %*
set "RC=%ERRORLEVEL%"
if not "%RC%"=="0" echo publish.sh 失敗（結束碼 %RC%）
goto :end

:fail
set "RC=1"

:end
if defined PAUSE_AT_END pause
if defined OLDCP chcp %OLDCP% >nul
endlocal & exit /b %RC%
