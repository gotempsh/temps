"""
Simple Flask application for testing Nixpacks deployment.

This app provides basic endpoints to verify the deployment is working correctly.
"""
from flask import Flask, jsonify
import os

app = Flask(__name__)


@app.route('/')
def hello():
    """Root endpoint returning a simple greeting."""
    return 'Hello from Nixpacks + Python Flask!'


@app.route('/health')
def health():
    """Health check endpoint for deployment verification."""
    return jsonify({
        'status': 'healthy',
        'framework': 'Flask',
        'python_version': os.sys.version,
        'environment': os.environ.get('FLASK_ENV', 'production')
    })


@app.route('/info')
def info():
    """Application info endpoint."""
    return jsonify({
        'name': 'simple-python',
        'version': '1.0.0',
        'deployed_with': 'nixpacks'
    })


if __name__ == '__main__':
    port = int(os.environ.get('PORT', 5000))
    app.run(host='0.0.0.0', port=port, debug=False)
