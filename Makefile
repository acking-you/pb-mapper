#!/bin/bash
TAG ?= dev

build-pb-mapper-server:
	bash ./scripts/build/pb-mapper-server.sh

build-pb-mapper-server-x86_64_musl:
	bash ./scripts/build/pb-mapper-server-x86_64_linux_musl.sh

release-pb-mapper-server-docker-image: build-pb-mapper-server
	bash ./scripts/release/pb-mapper-server-docker-image.sh

release-pb-mapper-server-x86-64-musl-docker-image: build-pb-mapper-server-x86_64_musl
	bash ./scripts/release/pb-mapper-server-x86-64-linux-musl-docker-image.sh

.PHONY: