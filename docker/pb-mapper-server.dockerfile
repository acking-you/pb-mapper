FROM debian:testing-slim

# set workspace
WORKDIR /pb-mapper-server

COPY ./target/release/pb-mapper-server .
COPY ./target/release/pb-mapper-server.sh .

RUN chmod +x ./pb-mapper-server
RUN chmod +x ./pb-mapper-server.sh

ENV PB_MAPPER_PORT=7666
ENV USE_IPV6=false
ENV USE_MACHINE_MSG_HEADER_KEY=true
EXPOSE $PB_MAPPER_PORT

ENTRYPOINT [ "./pb-mapper-server.sh" ]
