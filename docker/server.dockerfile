FROM debian:testing-slim

# 设置工作目录
WORKDIR /server

RUN mkdir conf

VOLUME ["/server/conf"]

ENV HOME=/server/conf

# 打包可执行文件
COPY ./target/release/server .

# 指定程序启动命令
CMD ["./server"]