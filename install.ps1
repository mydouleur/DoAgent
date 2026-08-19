# DoAgent Windows 安装：irm https://raw.githubusercontent.com/mydouleur/DoAgent/main/install.ps1 | iex
# 装到 %LOCALAPPDATA%\Programs\do（用户目录，免管理员），并加入用户 PATH。
# 注意：用 .NET API 改 PATH，不用 setx——setx 有 1024 字符上限，会截断长 PATH。
$ErrorActionPreference = "Stop"
$dir = "$env:LOCALAPPDATA\Programs\do"
New-Item -ItemType Directory -Force $dir | Out-Null
$url = "https://github.com/mydouleur/DoAgent/releases/latest/download/do-windows-x86_64.exe"
Write-Host "下载 $url …"
Invoke-WebRequest $url -OutFile "$dir\do.exe"

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
    Write-Host "已加入用户 PATH（新开的终端生效）"
}
Write-Host "已安装到 $dir\do.exe"
Write-Host "下一步：do 启动后执行  /setting -g url <API地址> 、/setting -g key <你的key> 、/setting -g model <模型名>"
