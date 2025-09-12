// Enhanced pure canvas thumbnail + animation recorder
(function(){
  const W=800,H=600;
  const btnPNG=q('#capturePng');
  const btnWEBP=q('#captureWebp');
  const btnBoth=q('#captureBoth');
  const btnAnimToggle=q('#toggleAnim');
  const btnMakeAnim=q('#makeAnim');
  const fpsInput=q('#fps');
  const secInput=q('#seconds');
  const loopInput=q('#loopCount');
  const qualityInput=q('#quality');
  const statusEl=q('#status');
  const preview=q('#preview');
  const animPreview=q('#animPreview');
  const frame=q('#captureFrame');

  let running=true; let lastPhase=0; let rafId; let startTime=performance.now();

  function q(s){return document.querySelector(s);} 
  function status(m){statusEl.textContent=m;}

  btnPNG.addEventListener('click',()=>captureStill('image/png'));
  btnWEBP.addEventListener('click',()=>captureStill('image/webp'));
  btnBoth.addEventListener('click', async()=>{ await captureStill('image/png'); setTimeout(()=>captureStill('image/webp'),180); });
  btnAnimToggle.addEventListener('click',()=>{running=!running; btnAnimToggle.textContent=running?'Pause Animation':'Resume Animation'; if(running){startTime=performance.now(); loop();} else cancelAnimationFrame(rafId);});
  btnMakeAnim.addEventListener('click', generateAnimation);

  function loop(){
    if(!running) return;
    const t=(performance.now()-startTime)/1000; // seconds
    const cycle=12; // 12s full pattern cycle
    lastPhase=(t%cycle)/cycle; // 0-1
    frame.style.setProperty('outline','2px solid rgba(255,152,0,'+(0.30+0.2*Math.sin(t*2))+')');
    rafId=requestAnimationFrame(loop);
  }
  loop();

  function drawFrame(ctx, phase){
    // Background gradient
    const grad=ctx.createLinearGradient(0,0,W,H); grad.addColorStop(0,'#462605'); grad.addColorStop(.58,'#2d1803'); grad.addColorStop(1,'#271503'); ctx.fillStyle=grad; ctx.fillRect(0,0,W,H);
    // Pattern (thin rotated pluses towards right)
    const cell=64; const shift=(phase*128)%cell; ctx.globalAlpha=.22;
    const MAX_ANGLE = 0.35; // ~20 degrees
    for(let y=-cell;y<H+cell;y+=cell){
      for(let x=-cell;x<W+cell;x+=cell){
        const cx = x+shift, cy = y+shift;
        const arm = cell*0.26;
        const thick = Math.max(1, cell*0.05);
        // rotation factor based on horizontal position (only rotate once inside visible width)
        const nx = Math.min(1, Math.max(0, cx / W));
        const angle = MAX_ANGLE * nx; // 0 on left -> MAX on right edge
        ctx.save();
        ctx.translate(cx, cy);
        ctx.rotate(angle);
        ctx.fillStyle = '#ff9800';
        // vertical arm
        ctx.fillRect(-thick/2, -arm, thick, arm*2);
        // horizontal arm
        ctx.fillRect(-arm, -thick/2, arm*2, thick);
        ctx.restore();
      }
    }
    ctx.globalAlpha=1;
    // Bottom fade
    const fade=ctx.createLinearGradient(0,0,0,H); fade.addColorStop(.58,'rgba(15,20,27,0)'); fade.addColorStop(1,'rgba(15,20,27,.82)'); ctx.fillStyle=fade; ctx.fillRect(0,0,W,H);
    // Border
    ctx.save(); ctx.strokeStyle='#ff9800'; ctx.lineWidth=4; ctx.setLineDash([14,12]); ctx.strokeRect(4,4,W-8,H-8); ctx.restore();
    // Title
    ctx.save(); ctx.font='600 72px system-ui,Segoe UI,Roboto'; ctx.fillStyle='#fff'; ctx.textAlign='center'; ctx.textBaseline='middle'; ctx.shadowColor='rgba(0,0,0,.55)'; ctx.shadowBlur=18; ctx.shadowOffsetY=4; ctx.fillText('JuiceBox',W/2,H/2-90); ctx.restore();
    // Subtitle
    ctx.save(); ctx.font='500 34px system-ui,Segoe UI,Roboto'; ctx.fillStyle='#ffb347'; ctx.textAlign='center'; ctx.textBaseline='middle'; ctx.shadowColor='rgba(0,0,0,.55)'; ctx.shadowBlur=10; ctx.shadowOffsetY=2; ctx.fillText('Temporary File Host',W/2,H/2-25); ctx.restore();
    // Tagline gradient shimmer
    ctx.save(); const gx=W/2+Math.sin(phase*2*Math.PI)*W*0.15; const tg=ctx.createLinearGradient(gx-220,0,gx+220,0); tg.addColorStop(0,'#ff9800'); tg.addColorStop(.5,'#ffc069'); tg.addColorStop(1,'#ff9800'); ctx.font='600 44px system-ui,Segoe UI,Roboto'; ctx.fillStyle=tg; ctx.textAlign='center'; ctx.textBaseline='middle'; ctx.shadowColor='rgba(0,0,0,.35)'; ctx.shadowBlur=6; ctx.shadowOffsetY=2; ctx.fillText('the silly file share.',W/2,H/2+45); ctx.restore();
    // Glow
    ctx.save(); ctx.globalCompositeOperation='overlay'; const glow=ctx.createRadialGradient(W/2,H/2,60,W/2,H/2,300); glow.addColorStop(0,'rgba(255,152,0,.35)'); glow.addColorStop(1,'rgba(255,152,0,0)'); ctx.fillStyle=glow; ctx.fillRect(0,0,W,H); ctx.restore();
  }

  async function captureStill(mime){
    status('Drawing...');
    const c=document.createElement('canvas'); c.width=W; c.height=H; const ctx=c.getContext('2d');
    drawFrame(ctx,lastPhase);
    let outMime=mime;
    if(outMime==='image/webp'){
      try { if(!c.toDataURL('image/webp').startsWith('data:image/webp')) outMime='image/png'; } catch(_){ outMime='image/png'; }
    }
    const quality = parseFloat(qualityInput.value)||0.97;
    const dataUrl=c.toDataURL(outMime==='image/png'?'image/png':'image/webp', quality);
    preview.src=dataUrl;
    download(dataUrl,`juicebox-thumb-${Date.now()}.${outMime==='image/png'?'png':'webp'}`);
    status('Done.');
  }

  async function generateAnimation(){
    const fps = clamp(parseInt(fpsInput.value,10)||24,5,60);
    const seconds = clamp(parseInt(secInput.value,10)||6,1,20);
    const loops = parseInt(loopInput.value,10)||0; // 0=infinite for playback attribute
    status(`Recording ${seconds}s @ ${fps}fps...`);

    // Offscreen canvas for drawing frames
    const canvas=document.createElement('canvas'); canvas.width=W; canvas.height=H; const ctx=canvas.getContext('2d');
    const stream=canvas.captureStream(fps);
    const rec=new MediaRecorder(stream,{mimeType: pickWebMMime()});
    const chunks=[]; rec.ondataavailable=e=>{ if(e.data.size) chunks.push(e.data); };
    rec.onstop=()=>{
      const blob=new Blob(chunks,{type:rec.mimeType});
      const url=URL.createObjectURL(blob);
      animPreview.style.display='block';
      animPreview.loop = loops===0; // infinite
      animPreview.src=url; animPreview.play();
      downloadBlob(blob,`juicebox-anim-${fps}fps-${seconds}s.webm`);
      status('Animation complete');
    };
    rec.start();
    const totalFrames = fps*seconds;
    // Render frames deterministically across one cycle portion (phase ties to absolute frame index)
    for(let i=0;i<totalFrames;i++){
      const phase = (i/totalFrames); // 0-1 single cycle; for continuous shimmer use modulo with >1 cycles
      drawFrame(ctx, phase);
      await waitFrameInterval(1000/fps);
    }
    rec.stop();
  }

  function waitFrameInterval(ms){ return new Promise(r=>setTimeout(r, ms)); }
  function clamp(v,min,max){ return v<min?min:v>max?max:v; }
  function pickWebMMime(){
    const opts=['video/webm;codecs=vp9','video/webm;codecs=vp8','video/webm'];
    return opts.find(t=>MediaRecorder.isTypeSupported(t))||'video/webm';
  }

  function download(dataUrl,name){ const a=document.createElement('a'); a.href=dataUrl; a.download=name; document.body.appendChild(a); a.click(); a.remove(); }
  function downloadBlob(blob,name){ const url=URL.createObjectURL(blob); const a=document.createElement('a'); a.href=url; a.download=name; document.body.appendChild(a); a.click(); setTimeout(()=>{URL.revokeObjectURL(url); a.remove();},1500); }
})();