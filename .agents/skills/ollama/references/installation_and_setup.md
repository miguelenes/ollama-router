# Ollama Installation and Setup

Reference material extracted from the `ollama` skill core. Full
installation, connection-validation, and troubleshooting walkthrough.

## Installation and Setup

### Step 1: Collect Ollama URL

**IMPORTANT**: Always ask users for their Ollama URL. Do not assume it's running locally.

Ask the user: "What is your Ollama server URL?"

Common scenarios:
- **Local installation**: `http://localhost:11434` (default)
- **Remote server**: `http://192.168.1.100:11434`
- **Custom port**: `http://localhost:8080`
- **Docker**: `http://localhost:11434` (if port mapped to 11434)

If the user says they're running Ollama locally or doesn't know the URL, suggest trying `http://localhost:11434`.

### Step 2: Check if Ollama is Installed

Before proceeding, verify if Ollama is installed and running at the provided URL. Users can check by visiting the URL in their browser or running:

```bash
curl <OLLAMA_URL>/api/version
```

If Ollama is not installed, guide users to install it:

**macOS/Linux:**
```bash
curl -fsSL https://ollama.com/install.sh | sh
```

**Windows:**
Download from https://ollama.com/download

**Docker:**
```bash
docker run -d -v ollama:/root/.ollama -p 11434:11434 --name ollama ollama/ollama
```

### Step 3: Start Ollama Service

Ensure Ollama is running:

**macOS/Linux:**
```bash
ollama serve
```

**Docker:**
```bash
docker start ollama
```

The service typically runs at `http://localhost:11434` by default.

### Step 4: Validate Connection

Use the validation script to test connectivity and list available models.

**IMPORTANT**: The script path is relative to the skill directory. When running the script, either:
1. Use the full path from the skill directory (e.g., `/path/to/ollama/scripts/validate_connection.py`)
2. Change to the skill directory first and then run `python scripts/validate_connection.py`

```bash
# Run from the skill directory
cd /path/to/ollama
python scripts/validate_connection.py <OLLAMA_URL>
```

Example with the user's Ollama URL:
```bash
cd /path/to/ollama
python scripts/validate_connection.py http://192.168.1.100:11434
```

The script will:
- Normalize the URL (remove any path components)
- Check if Ollama is accessible
- Display the Ollama version
- List all installed models with sizes
- Provide troubleshooting guidance if connection fails

**Success output:**
```
✓ Connection successful!
  URL: http://localhost:11434
  Version: Ollama 0.1.0
  Models available: 2

Installed models:
  - llama3.2 (4.7 GB)
  - mistral (7.2 GB)
```

**Failure output:**
```
✗ Connection failed: Connection refused
  URL: http://localhost:11434

Troubleshooting:
  1. Ensure Ollama is installed and running
  2. Check that the URL is correct
  3. Verify Ollama is accessible at the specified URL
  4. Try: curl http://localhost:11434/api/version
```
