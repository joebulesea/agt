# 限制容器只能連到允許的網域（squid）

Docker 本身無法只放行特定網域。做法：容器放在 `internal` 網路（不能直接連外），
只能經由 squid proxy 出去，proxy 只放行 `squid.conf` 列出的網域。

```sh
docker network create --internal agt-isolated
docker network create agt-egress
docker build -t agt-proxy -f docker/proxy/Dockerfile docker/proxy
docker run -d --name agt-proxy --restart unless-stopped \
  --network agt-egress agt-proxy
docker network connect agt-isolated agt-proxy
```

設定檔中的 Agent（以 `claude-docker` 為例）加上網路與 proxy 變數：

```toml
args = ["run", "-it", "--rm", "--init", "--network", "agt-isolated",
        "-e", "HTTPS_PROXY=http://agt-proxy:3128", "-e", "HTTP_PROXY=http://agt-proxy:3128",
        "-e", "NO_PROXY=localhost,127.0.0.1",
        # …其餘參數照原本的設定
]
```

驗證：在分頁內執行 `curl -sS -o /dev/null -w '%{http_code}\n' https://api.anthropic.com` 應該連得上；
`curl https://example.com` 應該失敗（403 / CONNECT 被拒）。

限制：
- 只擋網域，不檢查內容；允許的網域本身（例如 github.com）仍可被用來外傳資料。
- 不使用 proxy 設定的程式（直接用 socket）在 `internal` 網路中根本連不出去，這是預期行為。
- 各 Agent 實際需要的網域會隨版本變動；被擋時看 `docker logs agt-proxy` 補上。
- 這份範例我沒有在實際的 Docker 環境中測試過。
