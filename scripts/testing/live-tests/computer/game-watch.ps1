# Watch the game from outside, read-only: what the browser shows of it through UI
# Automation, a few times a second, one JSON line per look.
#
#   game-watch.ps1 -Out <file.jsonl> [-Seconds 1800]
#
# Each line: t (ms), game (a browser window shows the Skyline Flap tab), ready (the
# "ready" overlay is up), ended (the "flight ended" overlay is up), score (the
# score readout), final (the score the "flight ended" overlay shows), explorer
# (each File Explorer window as "<handle> <folder>"). Running is a game
# with neither overlay. The page exposes those elements by their HTML ids
# (readyOverlay, gameoverOverlay, scoreReadout). The watch ends after -Seconds or
# when <file.jsonl>.stop exists.
param([Parameter(Mandatory)][string]$Out, [double]$Seconds = 1800)

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$root = $A::RootElement
$windows = New-Object System.Windows.Automation.PropertyCondition ($A::ControlTypeProperty), ([System.Windows.Automation.ControlType]::Window)
$utf8 = New-Object System.Text.UTF8Encoding($false)

function ById($within, $id) {
    $condition = New-Object System.Windows.Automation.PropertyCondition ($A::AutomationIdProperty), $id
    $within.FindFirst($Scope::Descendants, $condition)
}

function Texts($element) {
    $element.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_ }
}

$stop = "$Out.stop"
$end = (Get-Date).AddSeconds($Seconds)
while ((Get-Date) -lt $end -and -not (Test-Path $stop)) {
    $record = [ordered]@{ t = [DateTimeOffset]::Now.ToUnixTimeMilliseconds(); game = $false; explorer = @() }
    try {
        foreach ($window in $root.FindAll($Scope::Children, $windows)) {
            $name = $window.Current.Name
            $class = $window.Current.ClassName
            if ($name -like 'DevTools*') {
                # Developer tools are not a way to play: the suite fails on this.
                $record.devtools = $true
            }
            if ($class -eq 'CabinetWClass') {
                # Every Explorer window by its handle and folder: the checker counts
                # only one that was not open at the first look.
                $record.explorer += "$($window.Current.NativeWindowHandle) $name"
            }
            if ($class -like 'Chrome_WidgetWin*' -and $name -like 'Skyline Flap*') {
                $record.game = $true
                $record.ready = $null -ne (ById $window 'readyOverlay')
                $over = ById $window 'gameoverOverlay'
                $record.ended = $null -ne $over
                $readout = ById $window 'scoreReadout'
                if ($readout) { $record.score = (Texts $readout | Select-Object -First 1) }
                if ($over) {
                    $texts = @(Texts $over)
                    $at = [array]::IndexOf($texts, 'SCORE')
                    if ($at -ge 0 -and $at + 1 -lt $texts.Count) { $record.final = $texts[$at + 1] }
                }
            }
        }
    } catch {
        $record.error = $_.Exception.Message
    }
    [System.IO.File]::AppendAllText($Out, ($record | ConvertTo-Json -Compress) + "`n", $utf8)
    Start-Sleep -Milliseconds 150
}
