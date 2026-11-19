@echo off
rem Usage: agt-pr.cmd [worktree] [-Yes] [-NoPr] [-Base <branch>]
rem Commit, push and open a PR for a claude-docker tab, using the commit message
rem and PR body that Claude wrote to .agt\COMMIT_MSG and .agt\PR_BODY.md.
rem Details: agt-pr.ps1 in the same folder.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0agt-pr.ps1" %*
exit /b %ERRORLEVEL%
