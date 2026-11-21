# 生产部署与公网访问指南 (Production Deployment Guide)

本项目支持两种主流生产部署方式：
1. **云服务器一键部署**：使用 `docker-compose.yml` 完整编排 `postgres` + `rust-server` + `nextjs-web`
2. **前后端解耦部署**：后端与数据库部署在 Railway / Fly.io / 自有 VPS，前端无服务器部署在 Vercel / Cloudflare Pages

---

## 方案一：Docker Compose 全栈一键部署（推荐）

适用于任意 Linux 云服务器（Ubuntu/Debian 22.04+）：

### 1. 准备服务器环境
```bash
# 安装 Docker 与 Docker Compose 插件
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker $USER
```

### 2. 克隆项目与配置环境变量
```bash
git clone https://github.com/iRroxy/Project-Rust.git
cd Project-Rust

cp .env.production.example .env
# 编辑配置真实密钥和强密码
nano .env
```

### 3. 一键编译并启动
```bash
docker compose up -d --build
```
启动后：
- 前端运行在 `:8080`（可配合 Nginx 反代绑定域名并配置 SSL）
- 后端运行在 `:3000`

---

## 方案二：Vercel 前端 + Railway / Fly.io 后端解耦部署

### 1. 部署后端 (Railway / Fly.io)
- **Railway**：连接本 GitHub 仓库，选择根目录下的 `Dockerfile.server` 进行部署；在 Railway 后台添加 PostgreSQL 插件，并在 Variables 中配置：
  - `DATABASE_URL`: `${{Postgres.DATABASE_URL}}`
  - `LLM_API_KEY`: 您的真实大模型 API 密钥
  - `CORS_ALLOW_ORIGINS`: `https://your-frontend.vercel.app`

### 2. 部署前端到 Vercel
- 在 Vercel 后台点击 **Import Project**，选择本仓库；
- **Root Directory** 设置为 `web`；
- **Environment Variables** 添加：
  - `NEXT_PUBLIC_API_BASE`: `https://your-railway-backend.up.railway.app`
- 点击 **Deploy**，约 1 分钟即可完成全自动 CI/CD 上线。

---

## 方案三：本地开发公网临时访问 (Cloudflare Tunnel / ngrok)

如果想用手机随时体验本地运行的全栈服务：

```powershell
# 使用 cloudflared 临时穿透前端 (8080 或 3000)
cloudflared tunnel --url http://127.0.0.1:3000
```
将生成的公网域名分享给手机浏览器即可直接访问。
