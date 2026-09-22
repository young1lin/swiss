# 44 — 给集成测试装 Docker：WSL 里的 dockerd，Windows 侧一个环境变量

> 这是 owner 自己动手的运行手册，配 [docs/44-integration-harness-spec.md](44-integration-harness-spec.md)
> D2。目标只有一个：Windows 上的 `cargo test -p swiss-it --features it` 能通过
> `DOCKER_HOST=tcp://127.0.0.1:2375` 起到 WSL 里的容器。不装 Docker Desktop。
> 机器现状（2026-09-22 查过）：WSL 发行版 `<distro>`，NAT 网络模式，systemd 已在跑
> （`systemctl` 能应答），cargo 1.98.1 在 WSL 里也有；Windows 与 WSL 两边都没有 `docker`。

## 0. 为什么是这条路

- CI 的 ubuntu runner 自带 dockerd，本机走同一条路（dockerd + `DOCKER_HOST`），集成测试在两边
  看到的是同一个世界。
- testcontainers（Rust crate）通过 Docker API 起容器，认 `DOCKER_HOST`；容器把端口发布在 WSL 的
  `127.0.0.1`，WSL2 的 localhost 转发让 Windows 侧的测试进程直接连 `127.0.0.1:<端口>`。
- 不用 Docker Desktop：多一个常驻托盘程序、多一套许可条款，而它能给的（GUI、Kubernetes）这里一样
  不需要。

## 1. WSL 里：装 docker，让它同时听 unix socket 和 127.0.0.1:2375

在 `<distro>` 的 shell 里（`wsl -d <distro>`）：

```bash
# 1) Ubuntu 自己仓库里的 docker（universe 的 docker.io）。不用 docker.com 的 apt 源：
#    26.04 的 codename 在那边可能还没有条目，而 testcontainers 对 dockerd 版本没有要求。
sudo apt update
sudo apt install -y docker.io docker-compose-v2

# 2) 自己进 docker 组，免 sudo（重新登录 shell 后生效）
sudo usermod -aG docker "$USER"

# 3) dockerd 额外监听 127.0.0.1:2375（明文，但只在回环上——Docker 的 socket 等于 root，
#    绝不能写 0.0.0.0）。Ubuntu 的 unit 用 -H fd:// 启动，和 daemon.json 里的 hosts 冲突，
#    所以走 systemd override 把 ExecStart 的 -H 去掉，由 daemon.json 说了算。
sudo mkdir -p /etc/docker
sudo tee /etc/docker/daemon.json >/dev/null <<'EOF'
{
  "hosts": ["unix:///var/run/docker.sock", "tcp://127.0.0.1:2375"]
}
EOF
sudo mkdir -p /etc/systemd/system/docker.service.d
sudo tee /etc/systemd/system/docker.service.d/override.conf >/dev/null <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/bin/dockerd --containerd=/run/containerd/containerd.sock
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now docker

# 4) 验证：两个入口都活着
docker version --format '{{.Server.Version}}'
curl -s http://127.0.0.1:2375/_ping ; echo      # 期望：OK
```

如果 `systemctl` 报 "System has not been booted with systemd"（本机不会，已查），在
`/etc/wsl.conf` 里加 `[boot]\nsystemd=true`，然后 Windows 侧 `wsl --shutdown` 再进。

## 2. Windows 侧：一个用户级环境变量

PowerShell（普通用户，一次性）：

```powershell
[Environment]::SetEnvironmentVariable('DOCKER_HOST', 'tcp://127.0.0.1:2375', 'User')
```

新开一个终端，验证 Windows → WSL 这条链：

```powershell
curl.exe -s http://127.0.0.1:2375/_ping       # 期望：OK
```

WSL2 NAT 模式默认 `localhostForwarding=true`，WSL 里绑定在 `127.0.0.1` 的端口从 Windows 用
`127.0.0.1` 就能到——这条对 2375 成立，对容器发布出来的数据库端口同样成立。

## 3. 预拉镜像（可选，省首跑的等待）

```bash
docker pull mysql:8.4
docker pull postgres:17
docker pull redis:7
docker pull testcontainers/ryuk:0.11.0     # testcontainers 的收尸容器；版本以 crate 当时的为准
```

## 4. 已知的坑

- **WSL 没在跑，dockerd 就没在跑。** 开机后第一次跑集成测试前 `wsl -d <distro> --exec true`
  拉起发行版即可；测试底座在连不上 `DOCKER_HOST` 时会把这一句打在失败信息里。
- **ryuk 要挂 docker socket。** 它在 WSL 里跑，挂的是 `/var/run/docker.sock`，与 Windows 侧用 TCP
  无关；不要设 `TESTCONTAINERS_RYUK_DISABLED`，否则中断的测试会留下僵尸容器。
- **端口是随机的。** 容器端口由 Docker 分配，测试从 API 读，不要在任何地方写死 3306/5432/6379；
  本机 19999 上真实的 mysql/redis 连接与之无关。
- **`DOCKER_HOST` 是用户级变量。** 由 Windows 服务或计划任务启动的进程看不到它——19999 的
  部署脚本在 agent shell 里跑，能看到；如果某天集成测试进了 `scripts/deploy.ps1` 的门禁而
  部署改成了服务启动，那一步要显式传。
- **代理。** WSL 启动时那句 "localhost proxy configuration was detected but not mirrored" 只是提醒
  NAT 模式不转发 Windows 的本地代理；`docker pull` 走 WSL 自己的网络，与之无关。拉不动镜像时在
  `/etc/systemd/system/docker.service.d/override.conf` 里加 `Environment="HTTPS_PROXY=..."`。

## 5. 卸载

```bash
sudo systemctl disable --now docker
sudo apt purge -y docker.io docker-compose-v2
sudo rm -rf /etc/docker /etc/systemd/system/docker.service.d /var/lib/docker
```

```powershell
[Environment]::SetEnvironmentVariable('DOCKER_HOST', $null, 'User')
```
