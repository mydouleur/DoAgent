# DoAgent Windows 卸载：iwr https://raw.githubusercontent.com/mydouleur/DoAgent/main/uninstall.ps1 | iex
# 删除 %LOCALAPPDATA%\Programs\do 整个文件夹（含配置/白名单/审计），并从用户 PATH 移除该目录。
$ErrorActionPreference = "Stop"
$dir = "$env:LOCALAPPDATA\Programs\do"
if (Test-Path $dir) {
    Remove-Item -Recurse -Force $dir
    Write-Host "已删除 $dir"
}
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -contains $dir) {
    $newPath = ($userPath -split ';' | Where-Object { $_ -ne $dir -and $_ -ne "" }) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host "已从用户 PATH 移除（新开的终端生效）"
}
Write-Host "卸载完成（各项目目录下的 .do/ 随项目自行处置）"
