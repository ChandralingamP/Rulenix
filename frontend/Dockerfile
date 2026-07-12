FROM node:22-bookworm-slim AS builder
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM nginx:1.27-alpine AS runtime
RUN addgroup -S rulenix && adduser -S -G rulenix -u 10001 rulenix \
    && mkdir -p /var/cache/nginx/client_temp /var/run/nginx \
    && chown -R rulenix:rulenix /var/cache/nginx /var/run/nginx /etc/nginx/conf.d
COPY nginx.conf /etc/nginx/nginx.conf
COPY --from=builder /app/dist /usr/share/nginx/html
USER rulenix
EXPOSE 8080
CMD ["nginx", "-g", "daemon off;"]
