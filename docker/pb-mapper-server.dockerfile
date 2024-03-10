FROM debian:testing-slim

# set workspace
WORKDIR /pb-mapper-server

COPY ./target/release/pb-mapper-server .
COPY ./target/release/pb-mapper-server.sh .

RUN chmod +x ./pb-mapper-server
RUN chmod +x ./pb-mapper-server.sh

ENV PB_MAPPER_PORT=7666
ENV USE_IPV6=false
EXPOSE $PB_MAPPER_PORT

ENTRYPOINT [ "./pb-mapper-server.sh" ]