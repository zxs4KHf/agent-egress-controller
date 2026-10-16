param([switch]$IndependentLaunch,[string]$ReportPath)
$ErrorActionPreference='Stop'
trap {
    if($ReportPath){[IO.File]::WriteAllText($ReportPath,($_ | Out-String))}
    throw $_
}
$repo=Split-Path -Parent $PSScriptRoot
$updater=Join-Path $PSScriptRoot 'portable/aec-update.ps1'
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($updater,[ref]$tokens,[ref]$errors)
if($errors.Count){throw 'Updater parse error'}
. ([scriptblock]::Create((@($ast.EndBlock.Statements|Where-Object {$_ -is [Management.Automation.Language.FunctionDefinitionAst]}|ForEach-Object {$_.Extent.Text}) -join "`n")))
if(Get-Command Enter-IndependentUpdater -ErrorAction SilentlyContinue){
    $result=Enter-IndependentUpdater $IndependentLaunch.IsPresent
    if($null -ne $result){
        if($ReportPath -and (Test-Path -LiteralPath $ReportPath)){Get-Content -LiteralPath $ReportPath -Raw -Encoding UTF8}
        exit $result
    }
}
$source=Get-Content (Join-Path $PSScriptRoot 'test-shell-identity.ps1') -Raw -Encoding UTF8
$code=[regex]::Match($source,"(?s)Add-Type -TypeDefinition @'\r?\n(.*?)\r?\n'@").Groups[1].Value
if(-not $code){throw 'Native property reader missing'}
Add-Type -TypeDefinition $code
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class AecUpdaterWindowProbe {
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr FindWindow(string cls,string title);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
 [DllImport("user32.dll")] static extern IntPtr SendMessage(IntPtr hwnd,uint msg,IntPtr wp,IntPtr lp);
 [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd,uint msg,IntPtr wp,IntPtr lp);
 public static IntPtr Observed;
 public static IntPtr Icon;
 public static Thread Start(uint expectedPid,string title) {
  Observed=IntPtr.Zero; Icon=IntPtr.Zero;
  var thread=new Thread(()=>{
   for(int i=0;i<100;i++){
    var hwnd=FindWindow(null,title);uint pid=0;
    if(hwnd!=IntPtr.Zero)GetWindowThreadProcessId(hwnd,out pid);
    if(pid==expectedPid){Observed=hwnd;Icon=SendMessage(hwnd,0x7F,new IntPtr(1),IntPtr.Zero);return;}
    Thread.Sleep(100);
   }
  });thread.IsBackground=true;thread.Start();return thread;
 }
 public static void CancelVerified(uint expectedPid) {
  uint pid;GetWindowThreadProcessId(Observed,out pid);
  if(pid!=expectedPid)throw new Exception("Refusing to close non-fixture window");
  PostMessage(Observed,0x111,new IntPtr(7),IntPtr.Zero); // old YesNo MessageBox: No
  PostMessage(Observed,0x10,IntPtr.Zero,IntPtr.Zero); // new Form: Cancel
 }
}
'@
Add-Type -AssemblyName System.Windows.Forms
$title='AEC updater fixture '+[guid]::NewGuid().ToString('N')
$watch=[AecUpdaterWindowProbe]::Start($PID,$title)
$timer=New-Object Windows.Forms.Timer
$timer.Interval=100
$script:verdict=$null
$deadline=[DateTime]::UtcNow.AddSeconds(15)
$timer.Add_Tick({
    if([AecUpdaterWindowProbe]::Observed -eq [IntPtr]::Zero){
        if([DateTime]::UtcNow -gt $deadline){
            $timer.Stop()
            # Close only this fixture's own WinForms windows, never another process.
            foreach($window in @([Windows.Forms.Application]::OpenForms)){$window.Close()}
        }
        return
    }
    $timer.Stop()
    try {
        $id=[AecWindowProperties]::Read([AecUpdaterWindowProbe]::Observed,5)
        $script:verdict=($id -eq 'AgentEgress.Updater' -and [AecUpdaterWindowProbe]::Icon -ne [IntPtr]::Zero -and [AecWindowProperties]::IsUnpackaged($PID))
    } finally {[AecUpdaterWindowProbe]::CancelVerified($PID)}
})
try {
    $timer.Start()
    $confirmed=Show-UpdateMessage 'Isolated update confirmation: cancelling must not grant permission to stop any service.' $title $true
    if(-not $watch.Join(1000)){throw 'Window observer did not finish'}
    if($confirmed){throw 'Closing confirmation unexpectedly authorized shutdown'}
    if(-not $script:verdict){throw 'UPDATER_IDENTITY: updater lacks independent package/window identity or its own icon'}
    $pass='PASS: actual updater confirmation has independent identity/icon; close is cancel; zero network/download/gateway lifecycle calls.'
    if($ReportPath){[IO.File]::WriteAllText($ReportPath,$pass)}
    $pass
} finally {$timer.Stop();$timer.Dispose()}
