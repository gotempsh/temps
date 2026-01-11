# Flask Basic Example

Simple Flask API server for testing Python deployments.

## Running Locally

```bash
# Install dependencies
pip install -r requirements.txt

# Run development server
python app.py

# Or with gunicorn (production)
gunicorn -w 4 -b 0.0.0.0:5000 app:app
```

## Endpoints

- `GET /` - Hello message with JSON response
- `GET /health` - Health check endpoint

## Environment Variables

- `PORT` - Server port (default: 5000)
