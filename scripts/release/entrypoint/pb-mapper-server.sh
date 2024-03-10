#!/bin/bash

# check `PB_MAPPER_PORT`
if [ -z "$PB_MAPPER_PORT" ]; then
  echo "Error: PB_MAPPER_PORT is not set."
  exit 1
fi

if [ "$USE_IPV6" = "true" ]; then
  echo "USE_IPV6 is set to true"
  ./pb-mapper-server -p $PB_MAPPER_PORT --
else
  echo "USE_IPV6 is set to false or is not set"
  ./pb-mapper-server -p $PB_MAPPER_PORT
fi