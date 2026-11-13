# First logon in the bench VM: VMware Tools (so the Mac can run things in here), OpenSSH as a
# second way in, no sleep, and a marker when done.
$ErrorActionPreference = 'Continue'
Start-Transcript C:\sill-setup.log
powercfg /change standby-timeout-ac 0
powercfg /change monitor-timeout-ac 0
New-Item C:\sill -ItemType Directory -Force | Out-Null
$tools = (Get-Volume | Where-Object FileSystemLabel -eq 'VMware Tools').DriveLetter
if ($tools) { Start-Process "$($tools):\setup.exe" -ArgumentList '/S /v "/qn REBOOT=R"' -Wait }
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service sshd -StartupType Automatic
Start-Service sshd
New-NetFirewallRule -Name sill-sshd -DisplayName 'OpenSSH (bench)' -Enabled True -Direction Inbound -Protocol TCP -Action Allow -LocalPort 22
'ready' | Out-File C:\sill\ready.txt
Stop-Transcript
