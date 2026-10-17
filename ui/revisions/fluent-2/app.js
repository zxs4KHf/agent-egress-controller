/* Shared presentation only. Networking and authentication stay in the tray backend. */
window.AEC = {
  icons:{shield:'M12 3 20 6v6c0 5-4 8-8 10-4-2-8-5-8-10V6Z',route:'M5 5h11m-4-4 4 4-4 4M19 19H8m4-4-4 4 4 4',plus:'M12 5v14M5 12h14',close:'m6 6 12 12M18 6 6 18',up:'m6 14 6-6 6 6',down:'m6 10 6 6 6-6',save:'M5 3h12l4 4v14H3V3Zm2 0v6h10V3M7 21v-8h10v8',refresh:'M20 11a8 8 0 1 0-2 6M20 4v7h-7',settings:'M4 7h16M4 17h16M8 4v6M16 14v6',agent:'M7 4h10v5H7ZM5 11h14v9H5Zm4 4h.01M15 15h.01M12 1v3',help:'M9 8a3 3 0 1 1 5 3c-2 1-2 2-2 3M12 18h.01M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0',back:'m10 5-7 7 7 7M3 12h18',copy:'M8 8h13v13H8ZM3 16V3h13',play:'m7 4 14 8-14 8Z',stop:'M5 5h14v14H5Z',check:'m5 12 4 4L19 6'},
  icon(name){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox','0 0 24 24');svg.setAttribute('aria-hidden','true');svg.classList.add('icon');const p=document.createElementNS(svg.namespaceURI,'path');p.setAttribute('d',this.icons[name]||this.icons.route);svg.append(p);return svg},
  decorate(root=document){root.querySelectorAll('[data-icon]').forEach(el=>{if(!el.querySelector('.icon'))el.prepend(this.icon(el.dataset.icon))})},
  async json(url,options={}){const response=await fetch(url,{cache:'no-store',...options});if(!response.ok)throw Error('本机服务请求失败（'+response.status+'）');return response.json()},
  navigate(tab){if(window.parent!==window)window.parent.postMessage({type:'aec:navigate',tab},location.origin);else {const token=new URLSearchParams(location.search).get('t');location.href='/app?t='+encodeURIComponent(token)+'&tab='+tab}}
};
document.addEventListener('DOMContentLoaded',()=>{
  AEC.decorate();
  document.addEventListener('click',event=>{const link=event.target.closest('a[data-nav]');if(!link)return;if(event.ctrlKey||event.metaKey||event.shiftKey||event.button!==0)return;event.preventDefault();AEC.navigate(link.dataset.nav)});
});
