var builder = WebApplication.CreateBuilder(args);

// Configure port from environment variable
var port = Environment.GetEnvironmentVariable("PORT") ?? "8080";
builder.WebHost.UseUrls($"http://0.0.0.0:{port}");

var app = builder.Build();

// Root endpoint
app.MapGet("/", () => new
{
    message = "Hello from .NET!",
    status = "healthy",
    version = "1.0.0"
});

// Health check endpoint
app.MapGet("/health", () => new
{
    status = "ok"
});

app.Run();
