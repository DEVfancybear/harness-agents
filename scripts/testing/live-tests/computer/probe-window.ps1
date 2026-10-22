# A window only the screen shows: a code the agent must read off a screenshot, a
# text box it must click into and type in, and a record of the Enter key.
#
#   probe-window.ps1 -CodeFile <file> -Out <dir>
#
# The code is read from -CodeFile, which is deleted at once, so it is on the
# screen and nowhere the agent could read it as text. Every change to the text
# box is written to <dir>\typed.txt (UTF-8), each Enter in it appends a line to
# <dir>\keys.txt, and <dir>\window.txt holds the window's screen rectangle.
param([Parameter(Mandatory)][string]$CodeFile, [Parameter(Mandatory)][string]$Out)

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$code = (Get-Content -Raw -Path $CodeFile).Trim()
Remove-Item -Force $CodeFile

$utf8 = New-Object System.Text.UTF8Encoding($false)
$form = New-Object System.Windows.Forms.Form
$form.Text = 'ha live check'
$form.Width = 560
$form.Height = 300
$form.StartPosition = 'CenterScreen'
$form.TopMost = $true
$form.BackColor = [System.Drawing.Color]::White

$label = New-Object System.Windows.Forms.Label
$label.Text = "CODE: $code"
$label.Font = New-Object System.Drawing.Font('Consolas', 28, [System.Drawing.FontStyle]::Bold)
$label.AutoSize = $true
$label.Location = New-Object System.Drawing.Point(24, 24)
$form.Controls.Add($label)

$hint = New-Object System.Windows.Forms.Label
$hint.Text = 'Type here:'
$hint.Font = New-Object System.Drawing.Font('Segoe UI', 12)
$hint.AutoSize = $true
$hint.Location = New-Object System.Drawing.Point(24, 110)
$form.Controls.Add($hint)

$box = New-Object System.Windows.Forms.TextBox
$box.Font = New-Object System.Drawing.Font('Segoe UI', 14)
$box.Width = 490
$box.Location = New-Object System.Drawing.Point(24, 140)
$form.Controls.Add($box)

$box.Add_TextChanged({ [System.IO.File]::WriteAllText((Join-Path $Out 'typed.txt'), $box.Text, $utf8) })
$box.Add_KeyDown({
    param($sender, $event)
    if ($event.KeyCode -eq [System.Windows.Forms.Keys]::Enter) {
        [System.IO.File]::AppendAllText((Join-Path $Out 'keys.txt'), "ENTER $($box.Text)`n", $utf8)
        $event.SuppressKeyPress = $true
    }
})
$form.Add_Shown({
    $form.Activate()
    $b = $form.Bounds
    [System.IO.File]::WriteAllText((Join-Path $Out 'window.txt'), "$($b.X) $($b.Y) $($b.Width) $($b.Height)", $utf8)
})
[void]$form.ShowDialog()
