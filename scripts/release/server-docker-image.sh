#!/bin/bash

SCRIPT_DIR=$(cd `dirname $0`; pwd)
PROJECT_DIR="$SCRIPT_DIR/../.."
DOCKER_DIR=$PROJECT_DIR/docker
DOCKER_RELEASE_DIR=$DOCKER_DIR/target/release

if [ $# -lt 1 ]; then
        echo "Usage: $0 <TAG>"
        exit 1
fi
TAG=$1

REPOSITORY=ackingliu/nat-convertor
IMAGE_ADDRESS=$REPOSITORY:$TAG
DOCKER_FILE=server.dockerfile

# prepare binaries
rm -rf $DOCKER_RELEASE_DIR > /dev/null 2>&1
mkdir -p $DOCKER_RELEASE_DIR
cp $PROJECT_DIR/target/release/server $DOCKER_RELEASE_DIR/

# change dir
cd $DOCKER_DIR

# build docker image
echo "1. Start build $IMAGE_ADDRESS"
sudo docker build -t $IMAGE_ADDRESS -f $DOCKER_FILE .

echo "2. Start push $IMAGE_ADDRESS. Note: Please to login your docker account "
sudo docker push $IMAGE_ADDRESS
