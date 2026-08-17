//! Category: runtime-addressed payload RPC and full-duplex frame IO.

use barracuda_rpc::{
    RpcAddress, RpcFrame, RpcLaneStorage, RpcMethod, RpcPayloadReader, RpcPayloadWriter,
    RpcRegistry, RpcResult, RpcStream, Streaming,
};
use futures_util::{future, stream};
use static_cell::ConstStaticCell;

static RPC_LANES: ConstStaticCell<RpcLaneStorage<2, 1_024, 4>> =
    ConstStaticCell::new(RpcLaneStorage::new());

struct TransferImage;

impl RpcMethod for TransferImage {
    const ADDRESS: &'static str = "media.transfer_image";
    type Request = [u8; 16];
    type Response = [u8; 16];
    type Error = [u8; 4];
    type Input = Streaming;
    type Output = Streaming;
}

async fn transfer_image(
    _context: barracuda_rpc::RpcContext,
    requests: RpcStream<RpcFrame<[u8; 16]>>,
) -> RpcResult<RpcStream<Result<[u8; 16], [u8; 4]>>> {
    let responses = stream::unfold(requests, |mut requests| async move {
        match requests.next().await {
            Some(Ok(request)) => {
                let response = request.view().copied().map(Ok);
                Some((response, requests))
            }
            Some(Err(error)) => Some((Err(error), requests)),
            None => None,
        }
    });
    Ok(RpcStream::new(responses))
}

async fn send_image(mut writer: RpcPayloadWriter) -> RpcResult<()> {
    // `write_all` splits unknown-sized input at the Method request-frame size.
    writer
        .write_all(b"image-chunk-0001image-chunk-0002")
        .await?;
    writer.close().await
}

async fn receive_image(mut reader: RpcPayloadReader) -> RpcResult<()> {
    while let Some(response) = reader.read().await? {
        match response {
            Ok(frame) => println!("image bytes: {:?}", frame.as_ref()),
            Err(error) => println!("method error bytes: {:?}", error.as_ref()),
        }
    }
    Ok(())
}

async fn run() -> RpcResult<()> {
    let registry = RpcRegistry::new(RPC_LANES.take());
    registry.register::<TransferImage, _>(transfer_image)?;

    // The two handles are independent so full-duplex Methods can apply
    // backpressure in both directions without requiring a background task.
    let address = RpcAddress::try_from(TransferImage::ADDRESS)?;
    let (writer, reader) = registry.client().call_payload(&address)?;
    let (send_result, receive_result) =
        future::join(send_image(writer), receive_image(reader)).await;
    send_result?;
    receive_result
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> RpcResult<()> {
    run().await
}
