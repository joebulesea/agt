# agt-pr.ps1：在主機上把 claude-docker 分頁的修改 commit、push 並建立 PR，
# commit 訊息與 PR 說明使用容器內 Claude 寫好的檔案。
#
# 容器內不能使用 git，Claude 改完程式後把說明寫在 worktree 的兩個檔案：
#   .agt/COMMIT_MSG   commit 訊息（第一行是標題，空一行後是內文）；有變更要 commit 時必須有
#   .agt/PR_BODY.md   PR 說明（Markdown）；沒有時用 commit 的內文
# 這兩個檔案只當成文字交給 git / gh，不會被執行。成功後會刪除，下一輪由 Claude 重寫。
#
# 用法（通常透過同目錄的 agt-pr.cmd）：
#   agt-pr.cmd [worktree 路徑] [-Yes] [-NoPr] [-Base <branch>]
#     worktree 路徑  省略時使用目前目錄
#     -Yes           不詢問，直接執行
#     -NoPr          只 commit，不 push、不建立 PR
#     -Base          PR 的目標 branch（預設為 GitHub 上的預設 branch）
#   已經有開啟中的 PR 時只 push，並用 PR_BODY.md 更新 PR 說明。
#
# 安全：worktree（包括其中的 .git 檔）由容器內的 Agent 控制。本腳本先確認 .git 指向主 repo 登記的
# gitdir，之後用 GIT_DIR / GIT_WORK_TREE 固定那個 gitdir；說明檔不可以是連結；也不 commit 巢狀 repo。
# 請執行主 repo（或另外複製出來）的這份腳本，不要執行 worktree 裡的副本。
#
# 本檔含中文，必須以 UTF-8 with BOM 儲存（Windows PowerShell 5.1 才不會讀成亂碼）。

param(
    [Parameter(Position = 0)][string]$Worktree = (Get-Location).Path,
    [switch]$Yes,
    [switch]$NoPr,
    [string]$Base,
    # agt 的 worktree_root；gitdir 不可以位在這裡面（那是各分頁 Agent 可以寫入的地方）
    [string]$WorktreeRoot = (Join-Path $HOME '.agt\worktrees')
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

function Fail([string]$msg) { Write-Host "[agt-pr] $msg" -ForegroundColor Red; exit 1 }
function Info([string]$msg) { Write-Host "[agt-pr] $msg" -ForegroundColor Cyan }

function Full([string]$p) { [IO.Path]::GetFullPath($p).TrimEnd('\', '/') }
function Test-Under([string]$child, [string]$parent) {
    ((Full $child) + '\').StartsWith((Full $parent) + '\', [StringComparison]::OrdinalIgnoreCase)
}
function Test-SamePath([string]$a, [string]$b) {
    [string]::Equals((Full $a), (Full $b), [StringComparison]::OrdinalIgnoreCase)
}
function Test-Link([string]$p) {
    $i = Get-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue
    $i -and ($i.Attributes -band [IO.FileAttributes]::ReparsePoint)
}
# 名稱不能叫 git：PowerShell 的函式優先於執行檔，會呼叫到自己
function Invoke-Git {
    & git.exe -c core.quotepath=false @args
    if ($LASTEXITCODE -ne 0) { Fail "git $($args -join ' ') 失敗（exit $LASTEXITCODE）" }
}

# ---- 1. 確認 worktree 與它的 gitdir ----
$Worktree = $Worktree.Trim('"')   # cmd 傳入 "C:\x\" 時結尾的 \" 會變成引號
if (-not (Test-Path -LiteralPath $Worktree -PathType Container)) { Fail "找不到目錄：$Worktree" }
$wt = Full (Resolve-Path -LiteralPath $Worktree).ProviderPath

$dotGit = Join-Path $wt '.git'
$item = Get-Item -LiteralPath $dotGit -Force -ErrorAction SilentlyContinue
if (-not $item) { Fail "$wt 不是 git worktree（找不到 .git）" }
if ($item.PSIsContainer -or (Test-Link $dotGit)) {
    Fail "$dotGit 不是一般檔案；這支腳本只處理 agt 建立的 worktree"
}
$line = Get-Content -LiteralPath $dotGit -TotalCount 1 -Encoding UTF8
if ($line -notmatch '^gitdir:\s*(.+?)\s*$') { Fail "$dotGit 的格式不符" }
$target = $Matches[1]
if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $wt $target }
$gitdir = Full $target

# gitdir 必須在 Agent 寫不到的地方（主 repo 的 .git/worktrees/<名稱>），並且反過來登記著這個 worktree
$untrusted = "$dotGit 指向 $gitdir，不是主 repo 登記的位置；可能被 Agent 改過，不處理"
if ((Test-Under $gitdir $wt) -or (Test-Under $gitdir (Split-Path $wt -Parent)) -or (Test-Under $gitdir $WorktreeRoot)) {
    Fail $untrusted
}
if ((Split-Path (Split-Path $gitdir -Parent) -Leaf) -ne 'worktrees') { Fail $untrusted }
$back = Join-Path $gitdir 'gitdir'
if (-not (Test-Path -LiteralPath $back -PathType Leaf)) { Fail $untrusted }
$backPath = (Get-Content -LiteralPath $back -TotalCount 1 -Encoding UTF8).Trim()
if (-not (Test-SamePath $backPath $dotGit)) { Fail $untrusted }
$common = Full (Split-Path (Split-Path $gitdir -Parent) -Parent)

# 之後的 git（以及 gh 內部呼叫的 git）都固定用這個 gitdir，不再讀 worktree 裡的 .git
$env:GIT_DIR = $gitdir
$env:GIT_WORK_TREE = $wt
Set-Location -LiteralPath $wt

# ---- 2. 讓 git 忽略 .agt/（寫在主 repo 的 info/exclude，所有 worktree 共用）----
$exclude = Join-Path $common 'info\exclude'
$hasRule = (Test-Path -LiteralPath $exclude) -and
    (Select-String -LiteralPath $exclude -Pattern '^/?\.agt/?\s*$' -Quiet)
if (-not $hasRule) {
    New-Item -ItemType Directory -Force -Path (Split-Path $exclude) | Out-Null
    [IO.File]::AppendAllText($exclude, "`n/.agt/`n")
    Info "已在 $exclude 加入 /.agt/（讓 git 忽略說明檔）"
}

# ---- 3. 讀取說明檔 ----
$agtDir = Join-Path $wt '.agt'
$msgFile = Join-Path $agtDir 'COMMIT_MSG'
$bodyFile = Join-Path $agtDir 'PR_BODY.md'
# 連結可能指向主機上的其他檔案（例如金鑰），內容會被當成 commit / PR 說明送出去
foreach ($p in @($agtDir, $msgFile, $bodyFile)) {
    if (Test-Link $p) { Fail "$p 是連結，不處理" }
}
foreach ($p in @($msgFile, $bodyFile)) {
    if ((Test-Path -LiteralPath $p -PathType Leaf) -and (Get-Item -LiteralPath $p).Length -gt 64KB) {
        Fail "$p 超過 64KB，不像是說明文字"
    }
}
$hasMsg = Test-Path -LiteralPath $msgFile -PathType Leaf
$hasBody = Test-Path -LiteralPath $bodyFile -PathType Leaf
if ($hasMsg -and -not ((Get-Content -LiteralPath $msgFile -Encoding UTF8) -match '\S')) {
    Fail "$msgFile 是空的"
}

$branch = & git.exe symbolic-ref --quiet --short HEAD
if ($LASTEXITCODE -ne 0 -or -not $branch) { Fail "HEAD 不在任何 branch 上，無法 push" }
$status = @(& git.exe -c core.quotepath=false status --porcelain --untracked-files=all)
if ($LASTEXITCODE -ne 0) { Fail "git status 失敗" }
$hasChanges = $status.Count -gt 0

if ($hasChanges -and -not $hasMsg) {
    Fail "有變更但找不到 $msgFile；請先請 Claude 寫好 commit 訊息"
}
if (-not $hasChanges -and $NoPr) { Info "沒有需要 commit 的變更"; exit 0 }

# ---- 4. 預覽並確認 ----
Info "worktree：$wt（branch $branch）"
if ($hasChanges) {
    Write-Host "`n---- 要 commit 的檔案 ----" -ForegroundColor Yellow
    $status | ForEach-Object { Write-Host "  $_" }
    Write-Host "`n---- commit 訊息 ----" -ForegroundColor Yellow
    Get-Content -LiteralPath $msgFile -Encoding UTF8 | ForEach-Object { Write-Host "  $_" }
} else {
    Info "沒有新的變更，只 push 並建立 / 更新 PR"
}
if (-not $NoPr) {
    Write-Host "`n---- PR 說明 ----" -ForegroundColor Yellow
    if ($hasBody) {
        Get-Content -LiteralPath $bodyFile -Encoding UTF8 | ForEach-Object { Write-Host "  $_" }
    } else {
        Write-Host "  （沒有 .agt/PR_BODY.md，使用 commit 內文）"
    }
}
Write-Host ''
if (-not $Yes) {
    $what = if ($NoPr) { 'commit' } else { 'commit、push 並建立 PR' }
    $ans = Read-Host "確定要$what 嗎？(y/N)"
    if ($ans -notmatch '^[yY]') { Info '已取消'; exit 1 }
}

# ---- 5. commit ----
if ($hasChanges) {
    Invoke-Git add -A
    # Agent 放進來的巢狀 repo 會被當成 gitlink；之後主機的 git 可能進到裡面讀它的 config，拒絕
    $nested = @(& git.exe -c core.quotepath=false diff --cached --raw --no-abbrev |
        Where-Object { $_ -match '^:000000 160000 ' } |
        ForEach-Object { ($_ -split "`t", 2)[1] })
    if ($nested.Count -gt 0) {
        Invoke-Git reset -q
        Fail "worktree 裡有巢狀的 git repo：$($nested -join '、')。請刪除其中的 .git（或移走該目錄）後再執行"
    }
    Invoke-Git commit -F $msgFile
    Remove-Item -LiteralPath $msgFile
}
if ($NoPr) { Info '完成（未 push）'; exit 0 }

# ---- 6. push 並建立 / 更新 PR ----
$remotes = @(& git.exe remote)
$remote = if ($remotes -contains 'origin') { 'origin' } else { $remotes | Select-Object -First 1 }
if (-not $remote) { Fail '這個 repo 沒有設定 remote，無法 push' }
Invoke-Git push -u $remote $branch

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Info "找不到 gh（GitHub CLI），已 push 到 $remote/$branch，請到 GitHub 上建立 PR"
    exit 0
}

# 已有開啟中的 PR：push 就會更新它，只需要更新說明
$ErrorActionPreference = 'Continue'   # Windows PowerShell 5.1 中導向 stderr 會被當成錯誤
$json = & gh pr view $branch --json url,state 2>$null
$viewed = $LASTEXITCODE -eq 0
$ErrorActionPreference = 'Stop'
$pr = if ($viewed -and $json) { ($json -join "`n") | ConvertFrom-Json } else { $null }

if ($pr -and $pr.state -eq 'OPEN') {
    if ($hasBody) {
        & gh pr edit $branch --body-file $bodyFile
        if ($LASTEXITCODE -ne 0) { Fail 'gh pr edit 失敗' }
        Remove-Item -LiteralPath $bodyFile
    }
    Info "已更新 PR：$($pr.url)"
    exit 0
}

# --fill-first：標題用 branch 上第一個 commit 的標題；--body-file 會蓋掉自動填入的內文
$ghArgs = @('pr', 'create', '--head', $branch, '--fill-first')
if ($Base) { $ghArgs += @('--base', $Base) }
if ($hasBody) { $ghArgs += @('--body-file', $bodyFile) }
& gh @ghArgs
if ($LASTEXITCODE -ne 0) { Fail 'gh pr create 失敗' }
if ($hasBody) { Remove-Item -LiteralPath $bodyFile }
Info '完成'
