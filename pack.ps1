# ============================================================
# wallet-m 源码打包脚本
# 打包预设文件集合: /src, build.sh, Cargo.lock, Cargo.toml
# ============================================================

$ErrorActionPreference = "Stop"
Push-Location $PSScriptRoot

$output = Join-Path $PSScriptRoot "wallet-flow-bot-src.tar.gz"

Write-Host "[*] 收集源码文件..." -ForegroundColor Cyan

# 预设打包文件集合
$files = @(
    "build.sh",
    "Cargo.lock",
    "Cargo.toml"
)

# 递归收集 src/ 下所有文件
Get-ChildItem -Path "src" -Recurse -File | ForEach-Object {
    $relative = $_.FullName.Substring((Get-Location).Path.Length + 1)
    $files += $relative
}

Write-Host "[*] 共 $($files.Count) 个文件" -ForegroundColor Gray

# 创建临时目录
$tmp = Join-Path $env:TEMP "wallet-flow-bot-pack-$pid"
if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
New-Item -ItemType Directory -Path $tmp | Out-Null

# 复制文件到临时目录
foreach ($f in $files) {
    $src = Join-Path $PSScriptRoot $f
    $dst = Join-Path $tmp $f
    if (-not (Test-Path $src)) {
        Write-Host "[!] 跳过不存在: $f" -ForegroundColor Yellow
        continue
    }
    $parent = Split-Path $dst -Parent
    if (-not (Test-Path $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    Copy-Item $src $dst -Force
}

# 打包 tar.gz
if (Test-Path $output) { Remove-Item $output -Force }
tar -czf $output -C $tmp .

# 清理
Remove-Item -Recurse -Force $tmp

$size = (Get-Item $output).Length / 1KB
Write-Host "[OK] 打包完成: $output ($([math]::Round($size,1)) KB)" -ForegroundColor Green
Write-Host ""

Pop-Location
