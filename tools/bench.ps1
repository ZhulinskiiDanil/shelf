# Compares capture-path metrics of Shelf builds. Moves the mouse and presses PrtScrn:
# do not touch the PC while it runs. Usage: powershell -File tools\bench.ps1 dist\shelf-m1.exe dist\shelf-m3.exe
param([Parameter(ValueFromRemainingArguments)] [string[]]$exes)
$drive = Join-Path $PSScriptRoot "drive-capture.ps1"
$log = "$env:APPDATA\Shelf\logs\shelf.log"
foreach ($exe in $exes) {
    Start-Process $exe; Start-Sleep 2
    $n = (Get-Content $log).Count
    1..3 | ForEach-Object { powershell -STA -File $drive | Out-Null }
    $new = Get-Content $log | Select-Object -Skip $n
    $m = $new | Select-String "hotkey->overlay ([\d.]+)" | ForEach-Object { [double]$_.Matches[0].Groups[1].Value } | Sort-Object
    $c = $new | Select-String "clipboard ([\d.]+)" | ForEach-Object { [double]$_.Matches[0].Groups[1].Value } | Sort-Object
    "{0}: overlay median {1} ms, max {2} ms | clipboard median {3} ms, max {4} ms" -f (Split-Path $exe -Leaf), $m[[int]($m.Count / 2)], $m[-1], $c[[int]($c.Count / 2)], $c[-1]
}
