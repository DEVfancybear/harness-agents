# Close what a computer-use test opened, and nothing the person had open before.
#
#   cleanup.ps1 -Snapshot <file>                 remember the windows and tabs open now
#   cleanup.ps1 -Before <file> [-Tabs a|b] [-Folders a|b] [-Windows a|b]
#
# With -Before it closes browser tabs whose title matches one of -Tabs (through
# each tab's own close button), File Explorer windows whose title matches one of
# -Folders, and windows titled one of -Windows, but only those not in the
# snapshot. Patterns are -like patterns separated by `|`.
param([string]$Snapshot = '', [string]$Before = '', [string]$Tabs = '', [string]$Folders = '', [string]$Windows = '')

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$Types = [System.Windows.Automation.ControlType]
$root = $A::RootElement
$windowCondition = New-Object System.Windows.Automation.PropertyCondition ($A::ControlTypeProperty), ($Types::Window)
$tabCondition = New-Object System.Windows.Automation.PropertyCondition ($A::ControlTypeProperty), ($Types::TabItem)
$buttonCondition = New-Object System.Windows.Automation.PropertyCondition ($A::ControlTypeProperty), ($Types::Button)

function Id($element) { ($element.GetRuntimeId() -join '.') }
function Matches($name, $patterns) {
    foreach ($pattern in ($patterns -split '\|')) { if ($pattern -and $name -like $pattern) { return $true } }
    $false
}

# Every top-level window and every browser tab, with the id it keeps while open.
$items = @()
foreach ($window in $root.FindAll($Scope::Children, $windowCondition)) {
    try {
        $items += [pscustomobject]@{ Kind = 'window'; Id = (Id $window); Name = $window.Current.Name; Class = $window.Current.ClassName; Element = $window }
        if ($window.Current.ClassName -like 'Chrome_WidgetWin*') {
            foreach ($tab in $window.FindAll($Scope::Descendants, $tabCondition)) {
                $items += [pscustomobject]@{ Kind = 'tab'; Id = (Id $tab); Name = $tab.Current.Name; Class = ''; Element = $tab }
            }
        }
    } catch { }
}

if ($Snapshot) {
    $items | ForEach-Object { $_.Id } | Set-Content -Path $Snapshot -Encoding utf8
    exit 0
}

$known = @{}
if ($Before -and (Test-Path $Before)) { Get-Content $Before | ForEach-Object { $known[$_] = $true } }
foreach ($item in $items) {
    if ($known.ContainsKey($item.Id)) { continue }
    try {
        if ($item.Kind -eq 'tab' -and (Matches $item.Name $Tabs)) {
            $button = $item.Element.FindFirst($Scope::Descendants, $buttonCondition)
            if ($button) {
                $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                "closed tab: $($item.Name)"
            }
        } elseif ($item.Kind -eq 'window' -and (($item.Class -eq 'CabinetWClass' -and (Matches $item.Name $Folders)) -or (Matches $item.Name $Windows))) {
            $item.Element.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
            "closed window: $($item.Name)"
        }
    } catch {
        "could not close $($item.Name): $($_.Exception.Message)"
    }
}
