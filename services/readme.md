## http2-server.service
### Usage
1. Please download binary to the `/root` directory and make sure it has execute permissions.
2. Move `xxx.service` to `/etc/systemd/system` so that the `systemctl` command can find the service.
3. Execute the following three commands:
    ```bash
    systemctl enable xxx # Enable boot-up
    systemctl start xxx # Start http2-server service
    ```
4. Execute `systemctl status xxx` to ensure that `xxx` is running properly.