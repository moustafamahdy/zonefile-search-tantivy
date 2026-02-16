module.exports = {
  apps: [{
    name: 'zonefile-api',
    script: '/root/zonefile-search-tantivy/start-api.sh',
    interpreter: '/bin/bash',
    cwd: '/root/zonefile-search-tantivy',
    instances: 1,
    exec_mode: 'fork',
    autorestart: true,
    watch: false,
    max_memory_restart: '2G',
    min_uptime: '10s',
    max_restarts: 10,
    restart_delay: 4000,
    env: {
      NODE_ENV: 'production',
      RUST_LOG: 'info'
    },
    error_file: '/root/zonefile-search-tantivy/logs/pm2-error.log',
    out_file: '/root/zonefile-search-tantivy/logs/pm2-out.log',
    log_file: '/root/zonefile-search-tantivy/logs/pm2-combined.log',
    time: true,
    merge_logs: true,
    kill_timeout: 5000,
    listen_timeout: 10000,
    shutdown_with_message: false
  }]
};
