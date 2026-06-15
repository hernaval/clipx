using ClipX.Core.Clipboard;
using ClipX.Core.Rpc;

Console.WriteLine("[ClipX.Core] Starting clipboard bridge...");

using var monitor = new ClipboardMonitor();
using var server = new PipeServer(monitor);

var cts = new CancellationTokenSource();
Console.CancelKeyPress += (_, e) =>
{
    e.Cancel = true;
    server.Stop();
    cts.Cancel();
};

try
{
    await server.RunAsync();
}
catch (OperationCanceledException)
{
    // shutdown
}

Console.WriteLine("[ClipX.Core] Stopped.");
