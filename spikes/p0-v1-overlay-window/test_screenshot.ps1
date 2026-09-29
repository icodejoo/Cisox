Set-Location -Path E:\workspaces\Cisox\spikes\p0-v1-overlay-window
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$proc = Start-Process -FilePath "E:\workspaces\Cisox\spikes\p0-v1-overlay-window\target\debug\p0-v1-overlay-window.exe" -PassThru
Start-Sleep -Seconds 2

$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$graphics = [System.Drawing.Graphics]::FromImage($bmp)
$graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$bmp.Save("E:\workspaces\Cisox\spikes\p0-v1-overlay-window\screenshot.png", [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bmp.Dispose()

Stop-Process -Id $proc.Id -Force
