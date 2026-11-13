# The bench on Windows: the same run as scripts/bench.sh, with the app and the synthetic host
# side by side in this folder. Results land in .\out.
#   powershell -ExecutionPolicy Bypass -File bench.ps1 [extra sill arguments]
$ErrorActionPreference = 'Continue'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $here
$out = Join-Path $here 'out'
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item "$out\host", "$out\device" -ItemType Directory -Force | Out-Null
$port = Get-Random -Minimum 20000 -Maximum 40000
$code = '482913557208'
$hostProc = Start-Process "$here\sill-testhost.exe" -ArgumentList "--port $port --dir `"$out\host`" --code $code --input-log --audio" -RedirectStandardOutput "$out\host.log" -RedirectStandardError "$out\host.err" -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
$o = ($out -replace '\\', '/')
$script = "pair:127.0.0.1:$port,$code;wait:streaming;sleep:0.6;select:desktop;wait:pictures,60;sleep:0.6;shot:$o/2-desktop.png;ui:apps;sleep:0.5;shot:$o/3-apps.png;ui:settings;sleep:0.5;shot:$o/4-settings.png;ui:none;select:first;sleep:0.9;press:4;press:25,m;text:hello;click:0.25,0.75;scroll:0.1;paste:from the bench;sleep:0.7;shot:$o/5-window.png;ui:bar;sleep:0.5;shot:$o/6-window-bar.png;ui:none;disconnect;wait:idle;sleep:0.3;shot:$o/7-macs.png;exit"
$appArgs = @('--config-dir', "$out\device", '--allow-loopback', '--no-discovery', '--null-sound', '--size', '1180x760', '--timeout', '60', '--auto', $script) + $args
$env:RUST_BACKTRACE = '1'
$app = Start-Process "$here\sill.exe" -ArgumentList ($appArgs | ForEach-Object { if ($_ -match '[ ;]') { "`"$_`"" } else { $_ } }) -RedirectStandardOutput "$out\app.log" -RedirectStandardError "$out\app.err" -PassThru
$null = $app.Handle   # without this PowerShell forgets the exit code of a redirected process
if (-not $app.WaitForExit(90000)) { $app.Kill(); 'the app did not leave in 90 s' | Out-File "$out\result.txt" -Append }
Start-Sleep -Milliseconds 500
Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue
$status = $app.ExitCode
$log = Get-Content "$out\host.log" -Raw -ErrorAction SilentlyContinue
$fail = @()
if ($status -ne 0) { $fail += "the app left with $status" }
foreach ($want in @(@('"hidUsage":4,"down":true', 'the A key'), @('hello', 'the typed text'), @('0.25', 'the click'), @('from the bench', 'the pasted text'))) {
  if (-not $log -or -not $log.Contains($want[0])) { $fail += "the host never saw $($want[1])" }
}
foreach ($shot in '2-desktop', '5-window', '7-macs') { if (-not (Test-Path "$out\$shot.png")) { $fail += "no screenshot $shot" } }
if ($fail.Count -eq 0) { 'BENCH PASSED' | Out-File "$out\result.txt" -Append -Encoding ascii } else { $fail | Out-File "$out\result.txt" -Append -Encoding ascii; 'BENCH FAILED' | Out-File "$out\result.txt" -Append -Encoding ascii }
