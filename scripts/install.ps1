<#
agt 安裝腳本（Windows x64，不需要系統管理員）
  irm https://raw.githubusercontent.com/joebulesea/agt/main/scripts/install.ps1 | iex

先設定環境變數可調整行為（因為用 iex 執行，沒有參數可傳）：
  $env:AGT_REPO        GitHub repo（預設 joebulesea/agt）
  $env:AGT_VERSION     指定版本（例如 v0.3.0），預設最新版
  $env:AGT_NO_SETUP=1  安裝後不執行 agt setup
#>

# 整個安裝包在函式裡：用 iex 執行時，$ErrorActionPreference 等設定不會留在使用者的 PowerShell 工作階段
function Install-Agt {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    function Fail($msg) { throw "agt 安裝失敗：$msg" }

    $repo = if ($env:AGT_REPO) { $env:AGT_REPO } else { 'joebulesea/agt' }
    if ($repo -notmatch '^[\w.-]+/[\w.-]+$') {
        Fail "AGT_REPO 必須是 <owner>/<repo>（目前是 $repo）"
    }
    if (-not [Environment]::Is64BitOperatingSystem) { Fail '目前只提供 x64 版本' }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        Write-Host '注意：偵測到 ARM64，將安裝 x64 版本（靠 Windows 11 的 x64 模擬執行）' -ForegroundColor Yellow
    }
    $target = 'x86_64-pc-windows-msvc'

    $version = $env:AGT_VERSION
    if (-not $version) {
        $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
    }
    if ($version -match '^\d') { $version = "v$version" }
    if ($version -notmatch '^v\d+\.\d+\.\d+([-+][\w.]+)?$') { Fail "無法判斷版本：$version" }

    $name = "agt-$version-$target"
    $base = "https://github.com/$repo/releases/download/$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("agt-install-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        Write-Host "==> 下載 $name"
        Invoke-WebRequest -UseBasicParsing "$base/$name.zip" -OutFile "$tmp\$name.zip"
        Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS"

        Write-Host '==> 核對 SHA256'
        $want = $null
        foreach ($line in Get-Content "$tmp\SHA256SUMS") {
            $p = $line -split '\s+', 2
            if ($p.Count -eq 2 -and $p[1].Trim().TrimStart('*') -eq "$name.zip") { $want = $p[0].ToLower() }
        }
        if (-not $want) { Fail "SHA256SUMS 裡找不到 $name.zip" }
        $got = (Get-FileHash "$tmp\$name.zip" -Algorithm SHA256).Hash.ToLower()
        if ($want -ne $got) { Fail "SHA256 不符（預期 $want，實際 $got），已中止" }

        Expand-Archive "$tmp\$name.zip" -DestinationPath $tmp
        $src = Join-Path $tmp $name
        if (-not ((Test-Path "$src\agt.exe") -and (Test-Path "$src\agtd.exe"))) { Fail '壓縮檔內容不完整' }

        $dir = Join-Path $env:APPDATA 'agt'
        New-Item -ItemType Directory -Force -Path $dir | Out-Null

        # 舊版 daemon 還在執行時先停掉。原生指令的 stderr 在 Stop 模式下會變成例外，所以改用 Continue 並自行檢查
        if (Test-Path "$dir\agt.exe") {
            $ErrorActionPreference = 'Continue'
            try { & "$dir\agt.exe" daemon stop -y 2>&1 | Out-Null } catch { }
            $ErrorActionPreference = 'Stop'
        }

        # 執行中的 exe 無法覆蓋，但可以改名：先清掉上次留下的 .old，再把舊檔改名
        Get-ChildItem $dir -Filter 'agt*.old' -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue }
        foreach ($f in 'agt.exe', 'agtd.exe') {
            $dest = Join-Path $dir $f
            if (Test-Path $dest) {
                $old = "$dest.old"
                if (Test-Path $old) { $old = "$dest.$([guid]::NewGuid().ToString('N').Substring(0,6)).old" }
                Move-Item $dest $old -Force
            }
            Copy-Item "$src\$f" $dest -Force
        }
        Write-Host "==> 已安裝 $version 到 $dir"

        # 加入使用者 PATH（已存在則略過）。直接讀寫登錄檔：保留 REG_EXPAND_SZ 與 %VAR%，
        # SetEnvironmentVariable 會把它們展開並改存成 REG_SZ。
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        $raw = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        $parts = @($raw -split ';' | Where-Object { $_ })
        $has = $parts | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') -or $_.TrimEnd('\') -ieq '%APPDATA%\agt' }
        if (-not $has) {
            $key.SetValue('Path', (($parts + $dir) -join ';'), 'ExpandString')
            # 設定再刪除一個暫時變數，藉此廣播 WM_SETTINGCHANGE，新開的終端機才會讀到新 PATH
            [Environment]::SetEnvironmentVariable('AGT_PATH_REFRESH', '1', 'User')
            [Environment]::SetEnvironmentVariable('AGT_PATH_REFRESH', $null, 'User')
            Write-Host '已把安裝位置加入使用者 PATH（已開啟的終端機需要重新開啟）'
        }
        $key.Close()
        if (($env:Path -split ';') -notcontains $dir) { $env:Path = "$env:Path;$dir" }

        if ($env:AGT_NO_SETUP -ne '1') {
            Write-Host '==> 執行 agt setup'
            $ErrorActionPreference = 'Continue'
            & "$dir\agt.exe" setup
        } else {
            Write-Host '完成。請執行 agt setup 檢查環境。'
        }
    }
    finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Agt
