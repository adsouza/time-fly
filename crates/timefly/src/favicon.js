export function setFavicon(svg) {
    const icon = document.querySelector('link[rel~="icon"]') ?? document.createElement('link');
    icon.rel = 'icon';
    icon.type = 'image/svg+xml';
    icon.href = 'data:image/svg+xml,' + encodeURIComponent(svg);
    if (!icon.isConnected) document.head.append(icon);
}
