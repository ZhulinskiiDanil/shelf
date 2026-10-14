Add-Type @"
using System; using System.Runtime.InteropServices;
public static class K { [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte sc, uint f, UIntPtr e);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint f,int x,int y,uint d,UIntPtr e); }
"@
function Key($vk){ [K]::keybd_event($vk,0,0,[UIntPtr]::Zero); [K]::keybd_event($vk,0,2,[UIntPtr]::Zero) }
Add-Type -AssemblyName System.Windows.Forms
for($i=0;$i -lt 3;$i++){
  [K]::SetCursorPos(300,300); Key 0x2C; Start-Sleep -Milliseconds 400
  [K]::mouse_event(2,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 50
  [K]::SetCursorPos(700,600); Start-Sleep -Milliseconds 50
  [K]::mouse_event(4,0,0,0,[UIntPtr]::Zero); Start-Sleep -Milliseconds 100
  [K]::keybd_event(0x11,0,0,[UIntPtr]::Zero); Key 0x43; [K]::keybd_event(0x11,0,2,[UIntPtr]::Zero)
  Start-Sleep -Milliseconds 300
  $img = [System.Windows.Forms.Clipboard]::GetImage(); "clip: $($img.Width)x$($img.Height)"
}
