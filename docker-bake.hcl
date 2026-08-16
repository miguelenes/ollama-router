// Source of truth for the three runtime images. GHA cache scope is shared so
// the router, mock, and agent targets reuse each other's cargo-chef cook.
//
//   docker buildx bake router   # or: docker build -t ollama-router:local .
//   docker buildx bake mock
//   docker buildx bake agent    # or: docker build --target agent .

group "default" {
  targets = ["router", "mock", "agent"]
}

target "router" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "router"
  cache-from = ["type=gha,scope=ollama-router"]
  cache-to   = ["type=gha,mode=max,scope=ollama-router,ignore-error=true"]
}

target "mock" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "mock"
  cache-from = ["type=gha,scope=ollama-router"]
  cache-to   = ["type=gha,mode=max,scope=ollama-router,ignore-error=true"]
}

target "agent" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "agent"
  cache-from = ["type=gha,scope=ollama-router"]
  cache-to   = ["type=gha,mode=max,scope=ollama-router,ignore-error=true"]
}
