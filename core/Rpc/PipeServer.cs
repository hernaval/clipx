using System.IO.Pipes;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Text.Json;
using ClipX.Core.Clipboard;
using ClipX.Core.Models;

namespace ClipX.Core.Rpc;

public sealed class PipeServer : IDisposable
{
    private const string PipeName = "clipboardsync";
    private readonly CancellationTokenSource _cts = new();
    private readonly ClipboardMonitor _monitor;
    private readonly List<StreamWriter> _subscribers = new();
    private readonly Lock _subscribersLock = new();

    public PipeServer(ClipboardMonitor monitor)
    {
        _monitor = monitor;
        _monitor.ClipboardTextChanged += OnClipboardChanged;
    }

    private void OnClipboardChanged(string text, string sourceProcess)
    {
        var evt = new ClipboardEvent("clipboard_event", text, "text", DateTime.UtcNow, sourceProcess);
        var json = JsonSerializer.Serialize(evt);
        BroadcastNotification(json);
    }

    private void BroadcastNotification(string json)
    {
        var notification = new JsonRpcRequest("2.0", "clipboard_event", null,
            JsonSerializer.Deserialize<JsonElement>(json));
        var line = JsonSerializer.Serialize(notification);

        lock (_subscribersLock)
        {
            var dead = new List<StreamWriter>();
            foreach (var writer in _subscribers)
            {
                try
                {
                    writer.WriteLine(line);
                    writer.Flush();
                }
                catch
                {
                    dead.Add(writer);
                }
            }
            foreach (var d in dead) _subscribers.Remove(d);
        }
    }

    public async Task RunAsync()
    {
        Console.WriteLine($"[ClipX.Core] Named pipe server starting on \\\\.\\pipe\\{PipeName}");
        while (!_cts.Token.IsCancellationRequested)
        {
            var pipe = CreatePipe();
            try
            {
                await pipe.WaitForConnectionAsync(_cts.Token);
                _ = Task.Run(() => HandleClient(pipe));
            }
            catch (OperationCanceledException)
            {
                pipe.Dispose();
                break;
            }
        }
    }

    private static NamedPipeServerStream CreatePipe()
    {
        var ps = new PipeSecurity();
        var currentUser = WindowsIdentity.GetCurrent().User!;
        ps.AddAccessRule(new PipeAccessRule(currentUser,
            PipeAccessRights.FullControl, AccessControlType.Allow));

        return NamedPipeServerStreamAcl.Create(
            "clipboardsync",
            PipeDirection.InOut,
            NamedPipeServerStream.MaxAllowedServerInstances,
            PipeTransmissionMode.Byte,
            PipeOptions.Asynchronous,
            0, 0, ps);
    }

    private async Task HandleClient(NamedPipeServerStream pipe)
    {
        using var _ = pipe;
        var reader = new StreamReader(pipe, Encoding.UTF8);
        var writer = new StreamWriter(pipe, Encoding.UTF8) { AutoFlush = true };

        lock (_subscribersLock)
        {
            _subscribers.Add(writer);
        }

        try
        {
            while (pipe.IsConnected && !_cts.Token.IsCancellationRequested)
            {
                var line = await reader.ReadLineAsync(_cts.Token);
                if (line == null) break;

                var response = HandleRequest(line);
                if (response != null)
                {
                    await writer.WriteLineAsync(response);
                    await writer.FlushAsync();
                }
            }
        }
        catch (Exception ex) when (ex is IOException or OperationCanceledException)
        {
            // client disconnected
        }
        finally
        {
            lock (_subscribersLock)
            {
                _subscribers.Remove(writer);
            }
        }
    }

    private string? HandleRequest(string json)
    {
        JsonRpcRequest? request;
        try
        {
            request = JsonSerializer.Deserialize<JsonRpcRequest>(json);
        }
        catch
        {
            return JsonSerializer.Serialize(new JsonRpcResponse("2.0", null, null,
                new JsonRpcError(-32700, "Parse error")));
        }

        if (request == null)
            return JsonSerializer.Serialize(new JsonRpcResponse("2.0", null, null,
                new JsonRpcError(-32600, "Invalid request")));

        object? result = request.Method switch
        {
            "get_clipboard" => ClipboardMonitor.GetClipboardText(),
            "set_clipboard" => HandleSetClipboard(request),
            "ping" => "pong",
            _ => null
        };

        if (request.Method != "get_clipboard" && request.Method != "set_clipboard" && request.Method != "ping")
        {
            return JsonSerializer.Serialize(new JsonRpcResponse("2.0", request.Id, null,
                new JsonRpcError(-32601, "Method not found")));
        }

        return JsonSerializer.Serialize(new JsonRpcResponse("2.0", request.Id, result, null));
    }

    private static object HandleSetClipboard(JsonRpcRequest request)
    {
        if (request.Params is JsonElement elem && elem.TryGetProperty("content", out var contentElem))
        {
            var content = contentElem.GetString();
            if (content != null && ClipboardMonitor.SetClipboardText(content))
                return new { success = true };
        }
        return new { success = false, error = "Missing or invalid 'content' param" };
    }

    public void Stop() => _cts.Cancel();

    public void Dispose()
    {
        _cts.Cancel();
        _monitor.ClipboardTextChanged -= OnClipboardChanged;
        _cts.Dispose();
    }
}
