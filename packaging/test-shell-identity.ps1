param(
    [Parameter(Mandatory=$true)][string]$ControlExe,
    [switch]$ExistingWindow
)
$ErrorActionPreference = 'Stop'
$ControlExe = (Resolve-Path -LiteralPath $ControlExe).Path
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AecWindowProperties {
 [StructLayout(LayoutKind.Sequential)] public struct Key { public Guid fmtid; public uint pid; }
 [StructLayout(LayoutKind.Explicit, Size=24)] public struct Value { [FieldOffset(0)] public ushort type; [FieldOffset(8)] public IntPtr pointer; }
 [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
 interface Store {
  void GetCount(out uint count); void GetAt(uint index, out Key key);
  void GetValue(ref Key key, out Value value); void SetValue(ref Key key, ref Value value); void Commit();
 }
 [DllImport("shell32.dll")] static extern int SHGetPropertyStoreForWindow(IntPtr hwnd, ref Guid iid, out Store store);
 [DllImport("ole32.dll")] static extern int PropVariantClear(ref Value value);
 public static string Read(IntPtr hwnd, uint id) {
  Guid iid=typeof(Store).GUID; Store store; Marshal.ThrowExceptionForHR(SHGetPropertyStoreForWindow(hwnd,ref iid,out store));
  try { var key=new Key {fmtid=new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),pid=id}; Value value; store.GetValue(ref key,out value);
   try { return value.type==31 ? Marshal.PtrToStringUni(value.pointer) : null; } finally { PropVariantClear(ref value); }
  } finally { Marshal.ReleaseComObject(store); }
 }
}
public sealed class AecShellPage : IDisposable {
 readonly System.Net.Sockets.TcpListener listener;
 readonly System.Threading.Thread worker;
 public readonly System.Threading.ManualResetEvent Loaded = new System.Threading.ManualResetEvent(false);
 public int Port { get { return ((System.Net.IPEndPoint)listener.LocalEndpoint).Port; } }
 public AecShellPage() {
  listener = new System.Net.Sockets.TcpListener(System.Net.IPAddress.Loopback,0); listener.Start();
  worker = new System.Threading.Thread(Serve); worker.IsBackground=true; worker.Start();
 }
 void Serve() {
  try { while(true) { using(var client=listener.AcceptTcpClient()) {
   client.ReceiveTimeout=2000; client.SendTimeout=2000;
   using(var stream=client.GetStream()) {
    var request=new byte[4096]; int count=stream.Read(request,0,request.Length);
    if(count==0) continue;
    var html="<!doctype html><title>Shell identity test</title><p>Isolated control-center shell test. No network services or credentials.</p>";
    var body=System.Text.Encoding.UTF8.GetBytes(html);
    var head=System.Text.Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: "+body.Length+"\r\nConnection: close\r\n\r\n");
    stream.Write(head,0,head.Length); stream.Write(body,0,body.Length); Loaded.Set();
   }
  } } } catch(System.Net.Sockets.SocketException) {} catch(System.IO.IOException) {} catch(ObjectDisposedException) {}
 }
 public void Dispose() { listener.Stop(); worker.Join(3000); Loaded.Dispose(); }
}
'@
$owned = $null
$page = $null
$failure = $null
try {
    if (-not $ExistingWindow) {
        if ([Diagnostics.Process]::GetProcessesByName('aec-control').Count) { throw 'Close no user windows: a control center already exists; use -ExistingWindow to inspect it.' }
        # Only the presentation process. No tray/core, credentials, system proxy or time-zone writes.
        $page = New-Object AecShellPage
        $owned = Start-Process -FilePath $ControlExe -ArgumentList ('http://127.0.0.1:'+$page.Port+'/app?t=shell-test') -PassThru -WindowStyle Hidden
        if (-not $page.Loaded.WaitOne(30000)) { throw 'The isolated loopback test page did not load.' }
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $matches = @([Diagnostics.Process]::GetProcessesByName('aec-control') | Where-Object { $_.MainModule.FileName -eq $ControlExe -and $_.MainWindowHandle -ne [IntPtr]::Zero -and $_.MainWindowTitle -eq 'Agent Egress Controller' })
        # Process.MainWindowHandle may expose an intermediate startup window. Wait for the
        # actual application identity, retaining a bounded failure for the old implementation.
        if ($matches.Count -eq 1 -and [AecWindowProperties]::Read($matches[0].MainWindowHandle,5) -eq 'AgentEgress.Controller') { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($matches.Count -ne 1) { throw 'Exactly one matching control window is required.' }
    $window = $matches[0]
    if ($owned -and $window.Id -ne $owned.Id) { throw 'The observed window is not the test-owned process.' }
    $appId = [AecWindowProperties]::Read($window.MainWindowHandle,5)
    if ($appId -ne 'AgentEgress.Controller') { throw 'Taskbar identity regression: window has no independent AgentEgress.Controller ID.' }
    $launch = [AecWindowProperties]::Read($window.MainWindowHandle,2)
    $expected = '"' + (Join-Path (Split-Path $ControlExe) 'aec-tray.exe') + '" --dashboard'
    if ($launch -ne $expected) { throw 'Taskbar relaunch must target the tray dashboard, never a session URL/token.' }
    $icon = [AecWindowProperties]::Read($window.MainWindowHandle,3)
    if ($icon -ne ($ControlExe+',0')) { throw 'Taskbar must use this installation control-center icon.' }
} catch {
    $failure = $_
} finally {
    if ($page) { $page.Dispose() }
    if ($owned -and -not $owned.HasExited) {
        $owned.Refresh()
        if ($owned.MainModule.FileName -ne $ControlExe) { throw 'Test process identity changed; refusing to close.' }
        [void]$owned.CloseMainWindow()
        if (-not $owned.WaitForExit(10000)) { throw ('Test window did not close normally; not force-killing it. Original check: '+$failure) }
    }
}
if ($failure) { throw $failure }
Write-Output 'PASS: real control window has independent taskbar ID, own icon, and token-free dashboard relaunch; any test-owned window exited normally.'
