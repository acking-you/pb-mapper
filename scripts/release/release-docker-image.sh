#!/bin/bash

# check `PROJECT_DIR`
if [ -z "$PROJECT_DIR" ]; then
  echo "Error: PROJECT_DIR is not set."
  exit 1
fi

# check `BIN_NAME`
if [ -z "$BIN_NAME" ]; then
  echo "Error: BIN_NAME is not set."
  exit 1
fi

# check `TARGET_NAME`
if [ -z "$TARGET_NAME" ]; then
  echo "TARGET_NAME is not set,we use target/release as TARGET_PATH"
  TARGET_PATH=$PROJECT_DIR/target/release
else
  echo "TARGET_NAME is set,we use target/$TARGET_NAME/release as TARGET_PATH"
  TARGET_PATH=$PROJECT_DIR/target/$TARGET_NAME/release
fi

DOCKER_DIR=$PROJECT_DIR/docker
echo "DOCKER_DIR:$DOCKER_DIR"
DOCKER_RELEASE_DIR=$DOCKER_DIR/target/release
echo "DOCKER_RELEASE_DIR:$DOCKER_RELEASE_DIR"
DOCKER_FILE=$BIN_NAME.dockerfile
echo "DOCKER_FILE:$DOCKER_FILE"

if [ $# -lt 1 ]; then
        echo "Usage: $0 <TAG>"
        exit 1
fi
TAG=$1

REPOSITORY=ackingliu/pb-mapper
IMAGE_ADDRESS=$REPOSITORY:$TAG
echo "image address:$IMAGE_ADDRESS"

# prepare binaries
rm -rf $DOCKER_RELEASE_DIR > /dev/null 2>&1
mkdir -p $DOCKER_RELEASE_DIR
echo "start cp $TARGET_PATH/$BIN_NAME $DOCKER_RELEASE_DIR/"
cp $TARGET_PATH/$BIN_NAME $DOCKER_RELEASE_DIR/
echo "start cp $PROJECT_DIR/scripts/release/entrypoint/$BIN_NAME.sh $DOCKER_RELEASE_DIR/"
cp $PROJECT_DIR/scripts/release/entrypoint/$BIN_NAME.sh $DOCKER_RELEASE_DIR/

# change dir
cd $DOCKER_DIR

# build docker image
echo "1. Start build $IMAGE_ADDRESS"
sudo docker build -t $IMAGE_ADDRESS -f $DOCKER_FILE .

echo "2. Start push $IMAGE_ADDRESS. Note: Please to login your docker account "
sudo docker login
sudo docker push $IMAGE_ADDRESS
