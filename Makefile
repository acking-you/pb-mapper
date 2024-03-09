#!/bin/bash
TAG ?= dev

build-server:
	bash ./scripts/build/server.sh

build-server-docker-image: build-server
	bash ./scripts/release/server-docker-image.sh ${TAG}
.PHONY: