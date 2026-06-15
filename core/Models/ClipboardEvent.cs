using System.Text.Json.Serialization;

namespace ClipX.Core.Models;

public record ClipboardEvent(
    [property: JsonPropertyName("type")] string Type,
    [property: JsonPropertyName("content")] string? Content,
    [property: JsonPropertyName("format")] string Format,
    [property: JsonPropertyName("timestamp")] DateTime Timestamp,
    [property: JsonPropertyName("source_app")] string SourceApp
);

public record JsonRpcRequest(
    [property: JsonPropertyName("jsonrpc")] string JsonRpc,
    [property: JsonPropertyName("method")] string Method,
    [property: JsonPropertyName("id")] int? Id,
    [property: JsonPropertyName("params")] System.Text.Json.JsonElement? Params
);

public record JsonRpcResponse(
    [property: JsonPropertyName("jsonrpc")] string JsonRpc,
    [property: JsonPropertyName("id")] int? Id,
    [property: JsonPropertyName("result")] object? Result,
    [property: JsonPropertyName("error")] JsonRpcError? Error
);

public record JsonRpcError(
    [property: JsonPropertyName("code")] int Code,
    [property: JsonPropertyName("message")] string Message
);
