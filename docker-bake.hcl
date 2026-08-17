// Source of truth for the three runtime images. Woodpecker pipelines override
// cache-from/to with type=local; local bake uses the builder default.
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
}

target "mock" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "mock"
}

target "agent" {
  context    = "."
  dockerfile = "Dockerfile"
  target     = "agent"
}
