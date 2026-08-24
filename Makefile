REGISTRY         ?=
REGISTRY_APP     ?=
REGISTRY_APP_TAG ?= 0.0.1

IMAGE        = $(REGISTRY)/$(REGISTRY_APP):$(REGISTRY_APP_TAG)
IMAGE_LATEST = $(REGISTRY)/$(REGISTRY_APP):latest

.DEFAULT_GOAL := lint
.PHONY: lint run darwin-oci-build darwin-oci-push

lint:
	cargo fmt-check
	cargo lint

run:
	cargo run -p yt-mcp-server

darwin_oci_build: guard-REGISTRY guard-REGISTRY_APP
	container build --os linux --arch amd64 -f docker/Dockerfile -t "$(IMAGE)"

darwin_oci_push: guard-REGISTRY guard-REGISTRY_APP
	container i tag "$(IMAGE)" "$(IMAGE_LATEST)"
	container i push "$(IMAGE)"
	container i push "$(IMAGE_LATEST)"

guard-%:
	@test -n "$($*)" || { echo "ERROR: variable $* is not set"; exit 1; }