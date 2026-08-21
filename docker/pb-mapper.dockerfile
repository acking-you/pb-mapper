FROM debian:testing-slim

WORKDIR /pb-mapper

COPY ./target/release/pb-mapper .
COPY ./target/release/pb-mapper.sh .

RUN chmod +x ./pb-mapper ./pb-mapper.sh

ENV PB_MAPPER_PORT=7666
ENV USE_IPV6=false
ENV USE_MACHINE_MSG_HEADER_KEY=false
VOLUME ["/var/lib/pb-mapper/auth"]
EXPOSE $PB_MAPPER_PORT

ENTRYPOINT [ "./pb-mapper.sh" ]
