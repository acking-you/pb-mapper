# pb-mapper Server - Network Tunneling Made Easy

![Docker Pulls](https://img.shields.io/docker/pulls/ackingliu/pb-mapper)
![Docker Image Size](https://img.shields.io/docker/image-size/ackingliu/pb-mapper)

A high-performance Rust-based network tunneling server that allows you to expose local services to clients over a public network. Perfect for accessing your home services from anywhere!

## 🚀 Quick Start

### Using Docker Run
```bash
docker run -d \
  --name pb-mapper \
  -p 7666:7666 \
  -e PB_MAPPER_PORT=7666 \
  -e USE_MACHINE_MSG_HEADER_KEY=true \
  -e RUST_LOG=error \
  ackingliu/pb-mapper:latest-x86_64_musl
```

### Using Docker Compose
```yaml
version: '3.8'
services:
  pb-mapper:
    container_name: pb-mapper
    image: ackingliu/pb-mapper:latest-x86_64_musl
    environment:
      - PB_MAPPER_PORT=7666
      - USE_IPV6=false
      - USE_MACHINE_MSG_HEADER_KEY=true
      - RUST_LOG=error
    ports:
      - "7666:7666"
    restart: unless-stopped
```

Save as `docker-compose.yml` and run:
```bash
docker-compose up -d
```

## 🔧 Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `PB_MAPPER_PORT` | `7666` | **Required** - Port for the pb-mapper server to listen on |
| `USE_IPV6` | `false` | Enable IPv6 support (`true`/`false`) |
| `USE_MACHINE_MSG_HEADER_KEY` | `true` | Derive `MSG_HEADER_KEY` from hostname + MAC and persist to `/var/lib/pb-mapper-server/msg_header_key` |
| `RUST_LOG` | `error` | Logging level (`error`, `warn`, `info`, `debug`, `trace`) |

⚠️ **Important**: `PB_MAPPER_PORT` must be set or the container will exit with an error.

## 📋 Ubuntu Deployment Guide

### Step 1: Install Docker
```bash
# Update package manager
sudo apt update

# Install Docker
sudo apt install -y docker.io docker-compose

# Add user to docker group (logout/login required)
sudo usermod -aG docker $USER

# Start and enable Docker service
sudo systemctl start docker
sudo systemctl enable docker
```

### Step 2: Create Docker Compose Configuration
```bash
# Create directory for pb-mapper
mkdir -p ~/pb-mapper
cd ~/pb-mapper

# Create docker-compose.yml
cat > docker-compose.yml << 'EOF'
version: '3.8'
services:
  pb-mapper:
    container_name: pb-mapper
    image: ackingliu/pb-mapper:latest-x86_64_musl
    environment:
      PB_MAPPER_PORT: 7666
      USE_IPV6: false
      USE_MACHINE_MSG_HEADER_KEY: true
      RUST_LOG: error
    ports:
      - "7666:7666"
    restart: unless-stopped
EOF
```

### Step 3: Configure Firewall (if enabled)
```bash
# Allow pb-mapper port through firewall
sudo ufw allow 7666/tcp

# Check firewall status
sudo ufw status
```

### Step 4: Deploy and Start
```bash
# Start pb-mapper server
docker-compose up -d

# Check if it's running
docker-compose ps

# View logs
docker-compose logs -f pb-mapper
```

### Step 5: Verify Installation
```bash
# Check if server is listening
netstat -tlnp | grep 7666

# Test connectivity (from another machine)
telnet your-server-ip 7666
```

## 🌐 How pb-mapper Works

pb-mapper creates secure tunnels between your local services and remote clients through a public server:

```
[Local Service] ↔ [pb-mapper-server-cli] ↔ [pb-mapper Server] ↔ [pb-mapper-client] ↔ [Remote Client]
     :8080              registers with           :7666            connects from          :9090
                        service-key "my-app"                      service-key "my-app"
```

### Example Usage Scenario

1. **Setup**: You have a web service running on your home computer at `localhost:8080`
2. **Register**: Use pb-mapper to register this service with key "my-web-app"
3. **Access**: From anywhere, connect to "my-web-app" through your pb-mapper server
4. **Result**: Access your home web service remotely on `localhost:9090`

## 🏗️ Architecture Support

pb-mapper supports all architectures that Rust supports, but currently we provide pre-built Docker images for:
- **linux/amd64** (x86_64) - `ackingliu/pb-mapper:*-x86_64_musl`
- **linux/arm64** (aarch64) - `ackingliu/pb-mapper:*-aarch64_musl`

For other architectures, you can build the image yourself using the provided Dockerfile in the [GitHub repository](https://github.com/acking-you/pb-mapper).

## 🔍 Available Tags

| Tag | Description |
|-----|-------------|
| `latest-x86_64_musl` | Latest stable x86_64 build (recommended) |
| `latest-aarch64_musl` | Latest stable ARM64 build |
| `latest` | Multi-arch manifest (automatically selects architecture) |
| `v1.x.x-x86_64_musl` | Specific version x86_64 build |
| `v1.x.x-aarch64_musl` | Specific version ARM64 build |
| `v1.x.x` | Specific version multi-arch manifest |

**Recommendation**: Use `latest-x86_64_musl` for x86_64 systems or `latest-aarch64_musl` for ARM64 systems for best compatibility.

## 🛡️ Security Considerations

- **Firewall**: Only expose port 7666 to trusted networks
- **Encryption**: Use the encryption features in client/server tools
- **Access Control**: Implement service key management strategy
- **Updates**: Regularly update to the latest version for security patches

## 📊 Monitoring and Logs

### View Real-time Logs
```bash
docker-compose logs -f pb-mapper
```

### Check Container Status
```bash
docker-compose ps
docker stats pb-mapper
```

### Debug Connection Issues
```bash
# Check if port is accessible
nc -zv your-server-ip 7666

# View detailed logs
docker-compose logs pb-mapper
```

## 🔧 Advanced Configuration

### Custom Port Configuration
```yaml
services:
  pb-mapper:
    image: ackingliu/pb-mapper:latest-x86_64_musl
    environment:
      PB_MAPPER_PORT: 8888  # Custom port
    ports:
      - "8888:8888"         # Update port mapping
```

### IPv6 Support
```yaml
services:
  pb-mapper:
    image: ackingliu/pb-mapper:latest-x86_64_musl
    environment:
      USE_IPV6: true
      PB_MAPPER_PORT: 7666
    ports:
      - "[::]:7666:7666"    # IPv6 binding
```

### Production Deployment with Restart Policy
```yaml
services:
  pb-mapper:
    image: ackingliu/pb-mapper:latest-x86_64_musl
    environment:
      PB_MAPPER_PORT: 7666
      RUST_LOG: warn
    ports:
      - "7666:7666"
    restart: unless-stopped
    deploy:
      resources:
        limits:
          memory: 128M
          cpus: '0.5'
```

## 🚨 Troubleshooting

### Container Won't Start
- **Error**: "PB_MAPPER_PORT is not set"
  - **Solution**: Set the `PB_MAPPER_PORT` environment variable

### Connection Refused
- **Check**: Container is running: `docker ps`
- **Check**: Port is exposed: `netstat -tlnp | grep 7666`
- **Check**: Firewall allows the port: `sudo ufw status`

### High Memory Usage
- Add memory limits to your docker-compose.yml
- Monitor with: `docker stats pb-mapper`

## 📖 Related Tools

To use this server, you'll also need:
- **pb-mapper-server-cli**: Register local services ([Download releases](https://github.com/acking-you/pb-mapper/releases))
- **pb-mapper-client-cli**: Connect to remote services ([Download releases](https://github.com/acking-you/pb-mapper/releases))
- **pb-mapper UI**: Cross-platform GUI for all operations ([Download releases](https://github.com/acking-you/pb-mapper/releases))

## 🔗 Links

- **GitHub Repository**: [github.com/acking-you/pb-mapper](https://github.com/acking-you/pb-mapper)
- **Documentation**: [Project README](https://github.com/acking-you/pb-mapper/blob/master/README.md)
- **Issues & Support**: [GitHub Issues](https://github.com/acking-you/pb-mapper/issues)
- **Releases**: [GitHub Releases](https://github.com/acking-you/pb-mapper/releases)

---

**Made with ❤️ using Rust** - High performance, memory safe network tunneling solution.
