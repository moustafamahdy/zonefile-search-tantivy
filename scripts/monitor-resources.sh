#!/bin/bash
# Monitor resources during indexing

INDEXER_PID=$1
LOG_FILE="resource-usage.log"

if [ -z "$INDEXER_PID" ]; then
    INDEXER_PID=$(pgrep -f "domain-indexer")
fi

echo "Monitoring PID: $INDEXER_PID"
echo "Time,CPU%,MEM%,MEM_MB,VSZ_MB,RSS_MB" > "$LOG_FILE"

while kill -0 "$INDEXER_PID" 2>/dev/null; do
    # Get process stats
    STATS=$(ps -p "$INDEXER_PID" -o %cpu,%mem,vsz,rss --no-headers 2>/dev/null)

    if [ -n "$STATS" ]; then
        CPU=$(echo "$STATS" | awk '{print $1}')
        MEM=$(echo "$STATS" | awk '{print $2}')
        VSZ=$(echo "$STATS" | awk '{printf "%.0f", $3/1024}')
        RSS=$(echo "$STATS" | awk '{printf "%.0f", $4/1024}')

        TIMESTAMP=$(date '+%H:%M:%S')
        echo "$TIMESTAMP,$CPU,$MEM,$RSS,$VSZ,$RSS"
        echo "$TIMESTAMP,$CPU,$MEM,$RSS,$VSZ,$RSS" >> "$LOG_FILE"
    fi

    sleep 10
done

echo ""
echo "=== Indexing Complete ==="
echo "Final stats saved to $LOG_FILE"
