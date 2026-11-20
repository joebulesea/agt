# agt 安裝腳本（Windows x64）
#   irm https://github.com/<owner>/agt/releases/latest/download/install.ps1 | iex
#
# 環境變數：
#   AGT_REPO      GitHub repo（預設 OWNER/agt，發布前請改成實際位置）
#   AGT_VERSION   指定版本（例如 0.3.0），預設最新版
#   AGT_NO_SETUP  設為 1 就不在最後執行 `agt setup`
# 不需要系統管理員；安裝到 %APPDATA%\agt（和設定檔放在一起）。
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.ServicePointManager]::SecurityProtocol

function Fail($msg) { throw "install: $msg" }

$repo = if ($env:AGT_REPO) { $env:AGT_REPO } else { 'OWNER/agt' }
if ($repo -like 'OWNER/*') { Fail '請設定 $env:AGT_REPO = "<owner>/<repo>"（腳本內的預設值還沒改）' }
if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    Fail '目前只提供 Windows x64 的預編譯版本'
}

if ($env:AGT_VERSION) {
    $version = $env:AGT_VERSION.TrimStart('v')
} else {
    # 讀最後一次轉址的網址，不呼叫 API
    $req = [System.Net.WebRequest]::Create("https://github.com/$repo/releases/latest")
    $req.AllowAutoRedirect = $false
    $res = $req.GetResponse()
    $loc = $res.Headers['Location']
    $res.Close()
    if (-not $loc -or $loc -notmatch '/tag/v(.+)$') { Fail '查不到最新版本' }
    $version = $Matches[1]
}

$name = "agt-$version-windows-x64.zip"
$base = "https://github.com/$repo/releases/download/v$version"
$dest = Join-Path $env:APPDATA 'agt'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("agt-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "下載 agt $version（windows-x64）…"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile (Join-Path $tmp $name)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')

    # 核對 SHA256：不符就中止
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "\s\*?$([regex]::Escape($name))$" } | Select-Object -First 1
    if (-not $line) { Fail "SHA256SUMS 裡找不到 $name" }
    $expected = ($line -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $name)).Hash.ToLower()
    if ($expected -ne $actual) { Fail "SHA256 不符（預期 $expected，實際 $actual），已中止" }

    Expand-Archive -Path (Join-Path $tmp $name) -DestinationPath (Join-Path $tmp 'x') -Force
    if (-not (Test-Path (Join-Path $tmp 'x\agt.exe'))) { Fail '壓縮檔內容不符預期（缺少 agt.exe）' }

    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    # 已經安裝且 daemon 在執行時先停止（會結束所有 Agent）
    $old = Join-Path $dest 'agt.exe'
    if (Test-Path $old) { try { & $old daemon stop -y *> $null } catch {} }

    # 執行中的 exe 不能覆蓋，但可以改名：先改名為 .old 再放新檔
    # 舊版有獨立的 agtd.exe（已併入 agt.exe）：刪除；使用中就留著
    Remove-Item -Force (Join-Path $dest 'agtd.exe'), (Join-Path $dest 'agtd.exe.old') -ErrorAction SilentlyContinue
    foreach ($f in 'agt.exe') {
        $target = Join-Path $dest $f
        if (Test-Path "$target.old") { Remove-Item -Force "$target.old" -ErrorAction SilentlyContinue }
        if (Test-Path $target) { Rename-Item -Force $target "$f.old" }
        Copy-Item -Force (Join-Path $tmp "x\$f") $target
        Remove-Item -Force "$target.old" -ErrorAction SilentlyContinue
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# 把安裝目錄加進使用者 PATH（已存在就略過）
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$parts = @($userPath -split ';' | Where-Object { $_ })
if ($parts -notcontains $dest) {
    [Environment]::SetEnvironmentVariable('Path', (($parts + $dest) -join ';'), 'User')
    Write-Host "已把 $dest 加入使用者 PATH（新開的終端機才會生效）"
}
$env:Path = "$env:Path;$dest"
Write-Host "已安裝到 $dest"

if ($env:AGT_NO_SETUP -ne '1') {
    & (Join-Path $dest 'agt.exe') setup
}
