//! BGRA->NV12 转换方案对比 spike 入口。每次调用只跑一个方案并输出一行 JSON，便于脚本汇总。
//!
//! 用法示例：
//!   recording-convert-spike info
//!   recording-convert-spike convert --res 1440 --method vp_opt
//!   recording-convert-spike chain --res 1080
//!   recording-convert-spike e2e --res 1440 --method vp_opt --async 4 --preset veryfast
//!   recording-convert-spike verify --res 1080
//!   recording-convert-spike dxgi-list

mod bench;
mod chain;
mod enc;
mod gpu;
mod info;
mod mf;
mod reference;
mod shader;
mod vp;

use std::collections::HashMap;

use windows::Win32::Graphics::Direct3D11::*;

use bench::{Conv, bench_convert};
use gpu::*;

/// 命令行参数（`--key value` 形式）。
struct Args {
    cmd: String,
    kv: HashMap<String, String>,
}

impl Args {
    fn parse() -> Self {
        let v: Vec<String> = std::env::args().skip(1).collect();
        let cmd = v.first().cloned().unwrap_or_default();
        let mut kv = HashMap::new();
        let mut i = 1;
        while i < v.len() {
            if let Some(k) = v[i].strip_prefix("--") {
                kv.insert(k.to_string(), v.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            } else {
                i += 1;
            }
        }
        Self { cmd, kv }
    }
    fn get(&self, k: &str, d: &str) -> String {
        self.kv.get(k).cloned().unwrap_or_else(|| d.to_string())
    }
    fn num(&self, k: &str, d: usize) -> usize {
        self.get(k, &d.to_string()).parse().unwrap_or(d)
    }
}

/// 内容模式：默认含噪声（最坏情况），`--content desktop` 为无噪声的桌面类内容。
fn noisy(a: &Args) -> bool {
    a.get("content", "noisy") != "desktop"
}

/// 分辨率标签 -> 尺寸。
fn res_size(r: &str) -> (u32, u32) {
    match r {
        "1080" => (1920, 1080),
        _ => (2560, 1440),
    }
}

/// 各方案输出 NV12 纹理需要的绑定标志。
pub fn bind_for(method: &str) -> u32 {
    let rt = D3D11_BIND_RENDER_TARGET.0 as u32;
    match method {
        "vp_opt" => rt | D3D11_BIND_VIDEO_ENCODER.0 as u32,
        "cs" | "cs_c" => rt | D3D11_BIND_UNORDERED_ACCESS.0 as u32,
        _ => rt,
    }
}

/// 创建某个方案的转换器。
fn make_conv(gpu: &Gpu, method: &str, size: (u32, u32), out: (u32, u32), src: &SourcePool) -> Res<Box<dyn Conv>> {
    if out != size && method != "vp_opt" && method != "ps" {
        return err(format!("{method} 不支持缩放输出"));
    }
    Ok(match method {
        "vp_base" => Box::new(vp::VpBase::new(gpu, size, size, 60, src.tex.clone())?),
        "vp_multi" => Box::new(vp::VpMulti::new(gpu, size, &src.tex, false)?),
        "vp_multi_tile" => Box::new(vp::VpMulti::new(gpu, size, &src.tex, true)?),
        "vp_opt" => Box::new(vp::VpOpt::new(gpu, size, out, 60, &src.tex, "")?),
        "vp_opt_rt" => Box::new(vp::VpOpt::new(gpu, size, out, 60, &src.tex, "_rt")?),
        "ps" => Box::new(shader::PsConv::new(gpu, out, &src.tex, true)?),
        "ps_c" => Box::new(shader::PsConv::new(gpu, out, &src.tex, false)?),
        "cs" => Box::new(shader::CsConv::new(gpu, size, &src.tex, true)?),
        "cs_c" => Box::new(shader::CsConv::new(gpu, size, &src.tex, false)?),
        other => return err(format!("未知方案 {other}")),
    })
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "'").replace(['\n', '\r'], " ")
}

fn cmd_convert(a: &Args) -> Res<String> {
    let res = a.get("res", "1440");
    let method = a.get("method", "vp_opt");
    let size = res_size(&res);
    let out = res_size(&a.get("out", &res));
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 8, false, noisy(a))?;
    let bind = bind_for(if method == "vp_opt_rt" { "vp_base" } else { &method });
    let dsts = nv12_ring(&gpu, out.0, out.1, 4, bind)?;
    let mut conv = make_conv(&gpu, &method, size, out, &src)?;
    let r = bench_convert(&gpu, conv.as_mut(), &dsts, a.num("warmup", 60), a.num("frames", 600))?;
    Ok(format!(
        "{{\"kind\":\"convert\",\"ok\":true,\"res\":\"{res}\",\"out\":\"{}\",\"method\":\"{method}\",{}}}",
        a.get("out", &res),
        r.json()
    ))
}

fn cmd_chain(a: &Args) -> Res<String> {
    let res = a.get("res", "1440");
    let size = res_size(&res);
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 8, false, noisy(a))?;
    let dsts = nv12_ring(&gpu, size.0, size.1, 4, bind_for("vp_base"))?;
    chain::bench_chain(&gpu, &res, size, &src.tex, &dsts, a.num("warmup", 60), a.num("frames", 600))
}

fn cmd_e2e(a: &Args) -> Res<String> {
    let res = a.get("res", "1440");
    let method = a.get("method", "vp_opt");
    let size = res_size(&res);
    let out = res_size(&a.get("out", &res));
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 8, false, noisy(a))?;
    let bind = match a.get("bind", "").as_str() {
        "rt" => bind_for("vp_base"),
        "rt_ve" => bind_for("vp_opt"),
        _ => bind_for(if method == "vp_opt_rt" { "vp_base" } else { &method }),
    };
    let opts = enc::QsvOpts {
        async_depth: a.num("async", 1) as u32,
        preset: a.get("preset", "medium"),
        bind,
        quality: a.num("quality", 23) as u32,
    };
    let mut pipe = enc::QsvPipe::new(&gpu, out.0, out.1, 60, &opts)?;
    let mut conv = make_conv(&gpu, &method, size, out, &src)?;
    let r = enc::bench_e2e(&gpu, &mut pipe, conv.as_mut(), a.num("warmup", 60), a.num("frames", 600))?;
    Ok(format!(
        "{{\"kind\":\"e2e\",\"ok\":true,\"res\":\"{res}\",\"out\":\"{}\",\"method\":\"{method}\",\"content\":\"{}\",\"async\":{},\"preset\":\"{}\",\"bind\":\"{}\",{}}}",
        a.get("out", &res),
        a.get("content", "noisy"),
        opts.async_depth,
        opts.preset,
        a.get("bind", "default"),
        r.json()
    ))
}

/// 正确性自校验：每个方案转换两帧并与 CPU 参考比较。
fn cmd_verify(a: &Args) -> Res<String> {
    let res = a.get("res", "1080");
    let size = res_size(&res);
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 2, true, true)?;
    let refs: Vec<_> = src.pixels.iter().map(|p| reference::reference(p, size.0, size.1)).collect();
    let mut items = Vec::new();
    for method in ["vp_base", "vp_opt", "ps", "ps_c", "cs", "cs_c", "mf"] {
        let item = (|| -> Res<String> {
            let mut cmps = Vec::new();
            if method == "mf" {
                let mut m = mf::MfConv::new(&gpu, size, &src.tex)?;
                for i in 0..2 {
                    let out = m.convert_once(i)?;
                    let got = read_nv12(&gpu, &out, size.0, size.1)?;
                    cmps.push(reference::compare(&got, &refs[i]));
                }
            } else {
                let dst = nv12_ring(&gpu, size.0, size.1, 1, bind_for(method))?;
                let mut conv = make_conv(&gpu, method, size, size, &src)?;
                for i in 0..2 {
                    conv.run(i, &dst[0])?;
                    gpu.wait_idle()?;
                    let got = read_nv12(&gpu, &dst[0], size.0, size.1)?;
                    cmps.push(reference::compare(&got, &refs[i]));
                }
            }
            let worst = |f: fn(&reference::Compare) -> f64| cmps.iter().map(f).fold(0.0, f64::max);
            let y_max = worst(|c| c.y.max);
            let uv_best = worst(|c| c.uv_center.max.min(c.uv_left.max));
            Ok(format!(
                "{{\"method\":\"{method}\",\"ok\":true,\"y_max_err\":{y_max:.2},\"uv_best_ref_max_err\":{uv_best:.2},\"frames\":[{}]}}",
                cmps.iter().map(|c| c.json()).collect::<Vec<_>>().join(",")
            ))
        })();
        items.push(item.unwrap_or_else(|e| format!("{{\"method\":\"{method}\",\"ok\":false,\"error\":\"{}\"}}", esc(&e.to_string()))));
    }
    Ok(format!("{{\"kind\":\"verify\",\"res\":\"{res}\",\"results\":[{}]}}", items.join(",")))
}

fn cmd_mf(a: &Args) -> Res<String> {
    let res = a.get("res", "1440");
    let size = res_size(&res);
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 8, false, noisy(a))?;
    let mut m = mf::MfConv::new(&gpu, size, &src.tex)?;
    let r = mf::bench_mf(&gpu, &mut m, a.num("warmup", 60), a.num("frames", 600))?;
    Ok(format!("{{\"kind\":\"convert\",\"ok\":true,\"res\":\"{res}\",\"method\":\"mf_vpmft\",{}}}", r.json()))
}

fn main() {
    let a = Args::parse();
    let r = match a.cmd.as_str() {
        "info" => info::info(),
        "dxgi-list" => info::dxgi_list(),
        "convert" => cmd_convert(&a),
        "chain" => cmd_chain(&a),
        "e2e" => cmd_e2e(&a),
        "verify" => cmd_verify(&a),
        "mf-convert" => cmd_mf(&a),
        "mf-probe" => mf::probe(),
        "mf-e2e" => mf::e2e(&a.get("res", "1440"), a.num("direct", 0) != 0, noisy(&a), a.num("warmup", 60), a.num("frames", 600)),
        other => Err(format!("未知命令 {other:?}").into()),
    };
    match r {
        Ok(s) => println!("{s}"),
        Err(e) => {
            println!("{{\"kind\":\"{}\",\"ok\":false,\"res\":\"{}\",\"method\":\"{}\",\"error\":\"{}\"}}", a.cmd, a.get("res", ""), a.get("method", ""), esc(&e.to_string()));
            std::process::exit(2);
        }
    }
}
