//! Quick diagnostic: connect to inverter and dump raw registers.

use givenergy_local::modbus::client::ModbusClient;
use givenergy_local::modbus::registers::STANDARD_POLL_BLOCKS;

#[tokio::main]
async fn main() {
    let host = "192.168.1.36";
    let port: u16 = 8899;
    let serial = "CE2052G072";

    println!("Connecting to {}:{} with serial {}...", host, port, serial);
    let mut client = ModbusClient::new(host, port, serial);

    match client.connect().await {
        Ok(()) => println!("Connected!"),
        Err(e) => {
            eprintln!("Connection failed: {}", e);
            return;
        }
    }

    for block in STANDARD_POLL_BLOCKS {
        let reg_type = match block.register_type {
            givenergy_local::modbus::registers::RegisterType::Input => "Input",
            givenergy_local::modbus::registers::RegisterType::Holding => "Holding",
        };
        println!(
            "\n=== {} block: {} start={} count={} ===",
            reg_type, block.name, block.start, block.count
        );

        let rt = match block.register_type {
            givenergy_local::modbus::registers::RegisterType::Input => {
                givenergy_local::modbus::framer::RegisterType::Input
            }
            givenergy_local::modbus::registers::RegisterType::Holding => {
                givenergy_local::modbus::framer::RegisterType::Holding
            }
        };

        match client.read_registers(rt, block.start, block.count).await {
            Ok(values) => {
                for (i, val) in values.iter().enumerate() {
                    let reg_addr = block.start as usize + i;
                    println!(
                        "  {:>3} (reg {:>3}): {:>6}  (0x{:04X})  signed: {:>6}",
                        i, reg_addr, val, val, *val as i16
                    );
                }
            }
            Err(e) => {
                eprintln!("  READ FAILED: {}", e);
            }
        }
    }

    client.disconnect().await;
    println!("\nDone.");
}
