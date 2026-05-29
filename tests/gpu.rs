use subforge::gpu::{GpuChoice, GpuInfo, choose_gpu, parse_nvidia_smi_csv};

#[test]
fn parse_nvidia_smi_csv_reads_index_name_and_memory() {
    let gpus = parse_nvidia_smi_csv("0, NVIDIA RTX 4090, 24564\n1, NVIDIA RTX 3090, 24576\n")
        .expect("valid csv should parse");

    assert_eq!(
        gpus,
        vec![
            GpuInfo {
                index: 0,
                name: "NVIDIA RTX 4090".into(),
                memory_mb: Some(24564)
            },
            GpuInfo {
                index: 1,
                name: "NVIDIA RTX 3090".into(),
                memory_mb: Some(24576)
            },
        ]
    );
}

#[test]
fn choose_gpu_uses_configured_index_when_present() {
    let gpus = vec![
        GpuInfo {
            index: 0,
            name: "GPU A".into(),
            memory_mb: None,
        },
        GpuInfo {
            index: 1,
            name: "GPU B".into(),
            memory_mb: None,
        },
    ];

    assert_eq!(
        choose_gpu(&gpus, "1").unwrap(),
        GpuChoice::Configured(gpus[1].clone())
    );
}

#[test]
fn choose_gpu_prompts_when_multiple_and_unconfigured() {
    let gpus = vec![
        GpuInfo {
            index: 0,
            name: "GPU A".into(),
            memory_mb: None,
        },
        GpuInfo {
            index: 1,
            name: "GPU B".into(),
            memory_mb: None,
        },
    ];

    assert_eq!(
        choose_gpu(&gpus, "").unwrap(),
        GpuChoice::NeedsSelection(gpus)
    );
}

#[test]
fn choose_gpu_auto_selects_single_detected_gpu() {
    let gpus = vec![GpuInfo {
        index: 0,
        name: "Only GPU".into(),
        memory_mb: Some(8192),
    }];

    assert_eq!(
        choose_gpu(&gpus, "").unwrap(),
        GpuChoice::Auto(gpus[0].clone())
    );
}
